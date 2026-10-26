//! The byte-range header of an HTTP request, as the desktop file scheme and the web file route both read it.

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
    fn what_is_not_one_plain_range_is_the_whole_file() {
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), Range::Whole, "several ranges");
        assert_eq!(parse_range(Some("items=0-1"), 100), Range::Whole, "another unit");
        assert_eq!(parse_range(Some("bytes=x-y"), 100), Range::Whole);
        assert_eq!(parse_range(Some("bytes=-0"), 100), Range::Whole);
        assert_eq!(parse_range(Some("bytes=0-5"), 0), Range::Whole, "an empty file has no ranges");
    }
}
