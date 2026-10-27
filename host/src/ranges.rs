//! The byte-range header of an HTTP request, as the desktop file scheme and the web file route both read it, and the two
//! sizes every route that serves a granted file is held to.

/// The most one ranged answer carries. The `file_range` command, the desktop file scheme and the file route of the server
/// all cut to it, so a large file is read in pieces and never whole.
pub const CHUNK: u64 = 8 * 1024 * 1024;

/// The most an answer without a range carries; a larger file must be asked for in ranges.
pub const WHOLE: u64 = 64 * 1024 * 1024;

/// A byte range of a file of `size` bytes: `start..=end`, both inside the file.
#[derive(Debug, PartialEq, Eq)]
pub enum Range {
    Whole,
    Bytes(u64, u64),
    /// The range cannot be satisfied (starts after the end).
    Unsatisfiable,
}

/// Read a `Range: bytes=...` header. One range only; anything else is the whole file, which a client must accept.
pub fn parse_range(header: Option<&str>, size: u64) -> Range {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else { return Range::Whole };
    if spec.contains(',') || size == 0 {
        return Range::Whole;
    }
    let Some((a, b)) = spec.split_once('-') else { return Range::Whole };
    let (a, b) = (a.trim(), b.trim());
    let last = size - 1;
    let (start, end) = match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(start), Ok(end)) => (start, end.min(last)),
        (Ok(start), Err(_)) if b.is_empty() => (start, last),
        // `-N`: the last N bytes.
        (Err(_), Ok(n)) if a.is_empty() && n > 0 => (size.saturating_sub(n), last),
        _ => return Range::Whole,
    };
    if start > last || start > end { Range::Unsatisfiable } else { Range::Bytes(start, end) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_read_and_clamped_to_the_file() {
        assert_eq!(parse_range(None, 100), Range::Whole);
        assert_eq!(parse_range(Some("bytes=0-"), 100), Range::Bytes(0, 99));
        assert_eq!(parse_range(Some("bytes=10-19"), 100), Range::Bytes(10, 19));
        assert_eq!(parse_range(Some("bytes=90-500"), 100), Range::Bytes(90, 99));
        assert_eq!(parse_range(Some("bytes=-10"), 100), Range::Bytes(90, 99));
        assert_eq!(parse_range(Some("bytes=-500"), 100), Range::Bytes(0, 99));
        assert_eq!(parse_range(Some("bytes=100-"), 100), Range::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=50-10"), 100), Range::Unsatisfiable);
    }

    #[test]
    fn the_chunk_is_smaller_than_the_whole_and_both_are_whole_mebibytes() {
        assert_eq!((CHUNK, WHOLE), (8 << 20, 64 << 20));
        assert!(CHUNK < WHOLE);
    }

    #[test]
    fn what_is_not_one_plain_range_is_the_whole_file() {
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), Range::Whole, "several ranges");
        assert_eq!(parse_range(Some("items=0-1"), 100), Range::Whole, "another unit");
        assert_eq!(parse_range(Some("bytes=x-y"), 100), Range::Whole);
        assert_eq!(parse_range(Some("bytes=-0"), 100), Range::Whole);
        assert_eq!(parse_range(Some("bytes=0-5"), 0), Range::Whole, "an empty file has no ranges");
    }
}
