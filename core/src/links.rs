//! Hyperlinks (OSC 8) as the engine hands them to the frame: bounded spans with a cleaned address.
//!
//! A program decides what an address is, so nothing it sends is trusted. Every address goes through [`clean_uri`]
//! before it can be a [`LinkSpan`]; an address that fails is no link and the text stays plain text. The core only
//! delivers: the allowlist of schemes of the settings, the `file` rules and the opening of a link belong to the link
//! policy in the host and to the page, and nothing here opens, resolves or fetches an address.
//!
//! # What a span is
//!
//! `[x0, x1, uri]` on one row, columns inclusive. Neighbouring cells with the same address are one span, a wide
//! character is covered by its span, and a link that wraps over two rows is two spans with the same address (one per
//! row); the page does not see a wrapped link as one object. That is a limit of this version.
//!
//! # Budgets
//!
//! [`MAX_URI_BYTES`] per address, [`MAX_SPANS_PER_ROW`] spans and [`MAX_URI_BYTES_PER_ROW`] address bytes per row (both
//! enforced by [`SpanBuilder`], which also tells the reader to stop), and [`MAX_URI_BYTES_PER_FRAME`] address bytes in
//! one frame (enforced by the frame encoder).

use crate::ai::tools::is_hidden;

/// The longest address that is accepted, in bytes. The engine itself keeps at most 2046 bytes of an address.
pub const MAX_URI_BYTES: usize = 2048;
/// The most spans one row carries.
pub const MAX_SPANS_PER_ROW: usize = 8;
/// The most address bytes the spans of one row carry together.
pub const MAX_URI_BYTES_PER_ROW: usize = 4096;
/// The most address bytes the links of one frame carry together; the encoder drops the links of the rows after it.
pub const MAX_URI_BYTES_PER_FRAME: usize = 128 * 1024;

/// The longest scheme, in characters.
const MAX_SCHEME_CHARS: usize = 32;

/// The fewest characters of a scheme name that a setting may list. A single letter is a drive letter on Windows
/// (`c:\dir\file.exe`), which the system's link handler would run, so no one-letter scheme is a scheme name here. An
/// address ([`clean_uri`]) still has the grammar of 1 to 32 characters: the core delivers, the page and the host decide.
const MIN_SCHEME_CHARS: usize = 2;

/// Schemes that are never delivered, whatever a setting says later: they run code or carry content, not a place.
/// Compared in ASCII lower case.
const DENIED_SCHEMES: [&str; 5] = ["javascript", "data", "vbscript", "blob", "about"];

/// A link on one viewport row: the columns `x0` to `x1` (inclusive) lead to `uri`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkSpan {
    pub x0: u16,
    pub x1: u16,
    pub uri: String,
}

/// Whether `name` is a scheme name as a setting may list it: 2 to 32 characters, a lower case letter first, then lower
/// case letters, digits, plus, dot or hyphen. An upper-case letter is not valid here (a list is kept in lower case),
/// while an address may carry its scheme in any case ([`clean_uri`]). One letter is never a scheme name here, so no
/// Windows drive letter (`c:`) can be listed; the host asks this again of the scheme of every link it opens.
pub fn valid_scheme(name: &str) -> bool {
    name.len() >= MIN_SCHEME_CHARS && !name.bytes().any(|b| b.is_ascii_uppercase()) && scheme_grammar(name.as_bytes())
}

/// Whether the text after the colon of a link (`rest`) starts an option for the program that handles the link: its
/// authority, or the part of it after the last `@` (the host), begins with a hyphen, written as `-` or as `%2d` in
/// either letter case. The authority is the text after an optional `//` up to the first `/`, `?` or `#`. A scheme
/// handler (`ssh` and others) may read such a text as an option (`ssh://-oProxyCommand=x@host/`), so the host refuses
/// it for every scheme but the web ones and `mailto`. A hyphen inside a host or a user name, in a path, a query or a
/// fragment is no option. Reads bytes only: nothing is decoded or allocated.
pub fn option_like_authority(rest: &str) -> bool {
    let bytes = rest.as_bytes();
    let after_slashes = bytes.strip_prefix(b"//").unwrap_or(bytes);
    let end = after_slashes.iter().position(|b| matches!(b, b'/' | b'?' | b'#')).unwrap_or(after_slashes.len());
    let authority = &after_slashes[..end];
    let host = authority.iter().rposition(|&b| b == b'@').map_or(authority, |at| &authority[at + 1..]);
    let dash = |text: &[u8]| text.first() == Some(&b'-') || (text.len() >= 3 && text[0] == b'%' && text[1] == b'2' && text[2].eq_ignore_ascii_case(&b'd'));
    dash(authority) || dash(host)
}

/// Whether `text` carries a character that a link must never have: a control character, a blank (space, tab, no-break
/// space, a line or paragraph separator, any other Unicode white space) or a format character (a direction mark or
/// override, a zero-width character, a soft hyphen, a tag character and the other invisible ones). Such a character makes
/// the text a person sees differ from the text that is opened. The invisible ones are decided by the one table the names
/// of the AI integration use, so the core has a single list of them. Ordinary letters outside ASCII (an internationalised
/// host, a name in any script) and emoji are not hidden. Empty text has none.
pub fn has_hidden_char(text: &str) -> bool {
    text.chars().any(|c| c.is_whitespace() || is_hidden(c))
}

/// Whether `name` is one of the schemes that are never delivered or opened: `javascript`, `data`, `vbscript`, `blob`
/// and `about`, in any letter case. The engine's cleaning and the settings of the link policy use this one list.
pub fn hard_denied(name: &str) -> bool {
    DENIED_SCHEMES.iter().any(|denied| name.eq_ignore_ascii_case(denied))
}

/// The grammar of a scheme in either letter case: 1 to 32 characters, a letter, then letters, digits, plus, dot or
/// hyphen.
fn scheme_grammar(scheme: &[u8]) -> bool {
    !scheme.is_empty()
        && scheme.len() <= MAX_SCHEME_CHARS
        && scheme[0].is_ascii_alphabetic()
        && scheme.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
}

/// The address a program sent, when it may be delivered: 1 to [`MAX_URI_BYTES`] bytes of printable ASCII (0x21 to 0x7E,
/// so no blank, no control character and no byte above 0x7E, which also keeps every bidirectional and other format
/// character out), a scheme of 1 to 32 characters (a letter, then letters, digits, plus, dot or hyphen) followed by a
/// colon, and not one of the schemes `javascript`, `data`, `vbscript`, `blob` and `about` in any letter case. The case
/// of the address is kept. `None` means: no link.
pub fn clean_uri(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > MAX_URI_BYTES {
        return None;
    }
    if !bytes.iter().all(|b| (0x21..=0x7E).contains(b)) {
        return None;
    }
    let colon = bytes.iter().position(|&b| b == b':')?;
    let scheme = &bytes[..colon];
    if !scheme_grammar(scheme) {
        return None;
    }
    // Printable ASCII only, checked above, so this is valid UTF-8.
    let text = String::from_utf8(bytes.to_vec()).ok()?;
    if hard_denied(&text[..colon]) {
        return None;
    }
    Some(text)
}

/// Builds the spans of one row cell by cell and enforces the per-row budgets.
///
/// The reader asks [`SpanBuilder::is_done`] before it reads an address and stops when it is true, so a row full of
/// links costs at most the budget. Cells must be offered in column order.
#[derive(Debug, Default)]
pub struct SpanBuilder {
    spans: Vec<LinkSpan>,
    bytes: usize,
    done: bool,
}

impl SpanBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// A budget stopped the row: no more address needs to be read.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// The cell at `x` carries `raw` (not yet cleaned). When the span that ends right before `x` has exactly this
    /// address the span grows and `true` is returned, so the address is neither cleaned nor allocated again.
    pub fn extend_if_same(&mut self, x: u16, raw: &[u8]) -> bool {
        match self.spans.last_mut() {
            Some(last) if x > 0 && last.x1 == x - 1 && last.uri.as_bytes() == raw => {
                last.x1 = x;
                true
            }
            _ => false,
        }
    }

    /// The cell at `x` has the cleaned address `uri`: it joins the span before it or starts a span, unless a budget
    /// (spans or address bytes of the row) says no, which ends the row.
    pub fn add(&mut self, x: u16, uri: String) {
        if self.extend_if_same(x, uri.as_bytes()) {
            return;
        }
        if self.spans.len() >= MAX_SPANS_PER_ROW || self.bytes + uri.len() > MAX_URI_BYTES_PER_ROW {
            self.done = true;
            return;
        }
        self.bytes += uri.len();
        self.spans.push(LinkSpan { x0: x, x1: x, uri });
    }

    /// The cell at `x` is the second half of a wide character (or another spacer): it is covered by the span of the
    /// cell before it.
    pub fn cover(&mut self, x: u16) {
        if let Some(last) = self.spans.last_mut()
            && x > 0
            && last.x1 == x - 1
        {
            last.x1 = x;
        }
    }

    pub fn finish(self) -> Vec<LinkSpan> {
        self.spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) -> bool {
        clean_uri(s.as_bytes()).is_some()
    }

    #[test]
    fn well_formed_addresses_are_kept_as_they_are() {
        for s in ["https://example.org/a?b=c%20d#e", "mailto:holz@example.org", "file:///tmp/a%20b", "HTTP://EXAMPLE.ORG"] {
            assert_eq!(clean_uri(s.as_bytes()).as_deref(), Some(s));
        }
    }

    #[test]
    fn length_is_one_to_2048_bytes() {
        assert!(clean_uri(b"").is_none());
        let long = format!("https://{}", "a".repeat(2048 - 8));
        assert_eq!(long.len(), 2048);
        assert!(ok(&long));
        assert!(!ok(&format!("{long}a")));
    }

    #[test]
    fn only_printable_ascii_passes() {
        for bad in [
            "https://a b",
            "https://a\tb",
            "https://a\nb",
            "https://a\rb",
            "https://a\x7fb",
            "https://a\x01b",
            "https://a\x1bb",
            "https://exämple.org",
            "https://a\u{202e}b",
            "https://a\u{200b}b",
            "https://a\u{00a0}b",
        ] {
            assert!(!ok(bad), "{bad:?}");
        }
        // A raw byte 0x80 and above is no ASCII at all, valid UTF-8 or not.
        assert!(clean_uri(b"https://a\x80b").is_none());
        assert!(clean_uri(b"https://a\xffb").is_none());
        // The whole printable range outside the scheme passes.
        let printable: String = (0x21u8..=0x7E).map(char::from).collect();
        assert!(ok(&format!("x:{printable}")));
    }

    #[test]
    fn the_scheme_has_a_grammar() {
        assert!(!ok("noscheme"));
        assert!(!ok("//example.org"));
        assert!(!ok(":x"));
        assert!(!ok("1http://x"));
        assert!(!ok("-http://x"));
        assert!(!ok("ht_tp://x"));
        assert!(!ok("ht tp://x"));
        assert!(ok("a+b.c-d1:x"));
        assert!(ok(&format!("{}:x", "a".repeat(32))));
        assert!(!ok(&format!("{}:x", "a".repeat(33))));
    }

    #[test]
    fn the_denied_schemes_fail_in_every_letter_case() {
        for s in ["javascript:alert(1)", "JaVaScRiPt:x", "JAVASCRIPT:x", "data:text/html,x", "DaTa:x", "vbscript:x", "VBScript:x", "blob:https://a/b", "BLOB:x", "about:blank", "About:blank"] {
            assert!(!ok(s), "{s}");
        }
        // Only the whole scheme counts.
        assert!(ok("javascripts:x"));
        assert!(ok("xdata:x"));
    }

    #[test]
    fn valid_scheme_is_the_lower_case_grammar_of_a_scheme_name() {
        for good in ["https", "ssh", "x-man-page", "a1+b.c", &"a".repeat(32)] {
            assert!(valid_scheme(good), "{good}");
        }
        for bad in ["", "a", "1http", "ht_tp", "-a", "+a", &"a".repeat(33), "HTTPS", "Ssh", "a b", "a:b", "a\tb", "h\u{e4}", "a/b"] {
            assert!(!valid_scheme(bad), "{bad:?}");
        }
    }

    #[test]
    fn has_hidden_char_finds_every_control_blank_and_format_character_and_nothing_else() {
        let mut hidden: Vec<char> = vec!['\u{ad}', '\u{61c}', '\u{feff}', '\u{e0001}', '\u{e0020}', '\u{0}', '\u{7}', '\u{1b}', '\u{7f}', '\u{9b}'];
        hidden.extend([' ', '\t', '\n', '\r', '\u{a0}', '\u{2003}', '\u{3000}', '\u{85}']);
        for range in [0x200B..=0x200F, 0x2028..=0x202E, 0x2060..=0x206F, 0xFFF9..=0xFFFB] {
            hidden.extend(range.map(|c| char::from_u32(c).expect("a scalar value")));
        }
        for c in hidden {
            for text in [format!("{c}"), format!("{c}https://a/"), format!("https://a/{c}b"), format!("https://a/{c}")] {
                assert!(has_hidden_char(&text), "{text:?}");
            }
        }
        let printable: String = (0x21u8..=0x7E).map(char::from).collect();
        for fine in ["", "https://example.org/a?b=c%20d#e", printable.as_str(), "\u{e4}", "\u{e9}", "\u{fc}", "\u{65e5}", "\u{44f}", "\u{1f600}", "https://ex\u{e4}mple.org/\u{65e5}"] {
            assert!(!has_hidden_char(fine), "{fine:?}");
        }
    }

    #[test]
    fn a_scheme_name_of_a_setting_has_two_to_32_characters_so_no_drive_letter_is_one() {
        for letter in 'a'..='z' {
            assert!(!valid_scheme(&letter.to_string()), "{letter}");
        }
        for good in ["ab", "ssh", "x-man-page", "a1+b.c", &"a".repeat(32)] {
            assert!(valid_scheme(good), "{good}");
        }
        assert!(valid_scheme("a1"));
        for bad in ["", "A", "1a"] {
            assert!(!valid_scheme(bad), "{bad}");
        }
        assert!(!valid_scheme(&"a".repeat(33)));
    }

    #[test]
    fn an_address_with_a_one_letter_scheme_is_still_delivered_the_page_and_the_host_decide() {
        assert_eq!(clean_uri(br"c:\Windows\System32\calc.exe").as_deref(), Some(r"c:\Windows\System32\calc.exe"));
        assert!(ok("C:/x.exe"));
        assert!(ok("x:y"));
        assert!(!ok("javascript:x"), "the denied schemes stay denied");
    }

    #[test]
    fn option_like_authority_finds_a_dash_that_a_handler_may_read_as_an_option() {
        for bad in [
            "//-oProxyCommand=x@host/",
            "-oProxyCommand=x",
            "//%2DoProxyCommand=x/",
            "//%2doProxyCommand=x/",
            "//user@-host/",
            "//user@%2dhost/",
            "//user@%2Dhost/",
            "//user:pw@-host/",
            "//a@b@-host/",
            "-x",
            "//-x",
            "//-",
            "-",
            "%2d",
            "//%2D",
            "//-host:22/path",
            "//-x?a=b",
            "//-x#f",
        ] {
            assert!(option_like_authority(bad), "{bad}");
        }
        for fine in [
            "//host.example/path",
            "//my-host.example",
            "//user-name@host.example/",
            "//user-name:pw-x@host.example/",
            "//host.example/-x",
            "//host.example/?a=-b",
            "//host.example#-x",
            "//host:22/",
            "//host.example/a@-b",
            "//host.example?u=a@-b",
            "x",
            "//",
            "",
            "//%2",
            "//%2e",
            "//%2fx",
            "//a%2d",
            "/-x",
        ] {
            assert!(!option_like_authority(fine), "{fine}");
        }
    }

    #[test]
    fn hard_denied_names_the_five_schemes_in_any_letter_case() {
        for denied in ["javascript", "data", "vbscript", "blob", "about", "JavaScript", "DATA", "VbScript", "Blob", "ABOUT"] {
            assert!(hard_denied(denied), "{denied}");
        }
        for fine in ["http", "https", "mailto", "file", "ssh", "javascripts", "xdata", "", "abou"] {
            assert!(!hard_denied(fine), "{fine}");
        }
    }

    #[test]
    fn spans_merge_neighbours_with_the_same_address_and_split_on_a_gap() {
        let mut b = SpanBuilder::new();
        b.add(0, "https://a/".into());
        b.add(1, "https://a/".into());
        b.cover(2);
        b.add(4, "https://a/".into());
        b.add(5, "https://b/".into());
        let spans = b.finish();
        assert_eq!(
            spans,
            vec![
                LinkSpan { x0: 0, x1: 2, uri: "https://a/".into() },
                LinkSpan { x0: 4, x1: 4, uri: "https://a/".into() },
                LinkSpan { x0: 5, x1: 5, uri: "https://b/".into() },
            ]
        );
    }

    #[test]
    fn a_cover_without_a_span_before_it_does_nothing() {
        let mut b = SpanBuilder::new();
        b.cover(0);
        b.cover(3);
        b.add(5, "https://a/".into());
        b.cover(7);
        assert_eq!(b.finish(), vec![LinkSpan { x0: 5, x1: 5, uri: "https://a/".into() }]);
    }

    #[test]
    fn the_ninth_span_ends_the_row() {
        let mut b = SpanBuilder::new();
        for i in 0..40u16 {
            if b.is_done() {
                break;
            }
            b.add(i * 2, format!("https://x/{i}"));
        }
        assert!(b.is_done());
        let spans = b.finish();
        assert_eq!(spans.len(), MAX_SPANS_PER_ROW);
        assert_eq!(spans[7].uri, "https://x/7");
    }

    #[test]
    fn the_address_bytes_of_a_row_are_bounded() {
        let uri = |i: u16| format!("https://x/{i}{}", "a".repeat(1500 - 10));
        let mut b = SpanBuilder::new();
        for i in 0..3u16 {
            b.add(i * 2, uri(i));
        }
        assert!(b.is_done());
        let spans = b.finish();
        assert_eq!(spans.len(), 2);
        assert!(spans.iter().map(|s| s.uri.len()).sum::<usize>() <= MAX_URI_BYTES_PER_ROW);
    }
}
