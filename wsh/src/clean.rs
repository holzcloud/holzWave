//! The one function that makes text from the app safe to print.
//!
//! The model's text and the messages of the app are untrusted: a terminal acts on escape sequences (a clipboard write, a
//! title change, a forged shell mark, a cursor move). The escape character itself is removed, so what follows it prints as
//! harmless text. Every other control character goes too, except the newline and the tab, and so do the characters that
//! change what a line looks like without showing (direction controls, zero-width characters, the tag block). The joiners
//! U+200C and U+200D and the variation selectors U+FE0E and U+FE0F stay, so emoji and some scripts still render. The table
//! is here and only here.

/// Whether `c` is removed from text that is printed: every control character except newline and tab (this covers the C0
/// range with ESC, BEL and the carriage return, DEL, and the C1 range U+0080 to U+009F), the soft hyphen, the Arabic letter
/// mark, the zero-width space, the left-to-right and right-to-left marks, the embedding and override controls, the word
/// joiner, the isolate controls, the byte order mark, the tag block and the variation selector supplement.
pub fn is_removed(c: char) -> bool {
    if c == '\n' || c == '\t' {
        return false;
    }
    c.is_control()
        || matches!(
            u32::from(c),
            0xAD | 0x61C | 0x200B | 0x200E | 0x200F | 0x202A..=0x202E | 0x2060 | 0x2066..=0x2069 | 0xFEFF | 0xE0000..=0xE007F | 0xE0100..=0xE01EF
        )
}

/// `text` without the removed characters. `removed` grows by one for each removed character except the carriage return,
/// which is removed without being counted because text with Windows line ends is common (a lone one would rewrite the line
/// the terminal already shows).
pub fn clean(text: &str, removed: &mut usize) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if !is_removed(c) {
            out.push(c);
        } else if c != '\r' {
            *removed += 1;
        }
    }
    out
}

/// `text` cleaned, for a message that is printed as it is.
pub fn text(s: &str) -> String {
    clean(s, &mut 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_lose_their_escape_and_the_rest_is_plain_text() {
        let mut removed = 0;
        let clipboard = clean("\u{1b}]52;c;aGVsbG8=\u{7}", &mut removed);
        let title = clean("\u{1b}]0;title\u{7}", &mut removed);
        assert_eq!((clipboard.as_str(), title.as_str()), ("]52;c;aGVsbG8=", "]0;title"));
        assert!(!clipboard.contains(['\u{1b}', '\u{7}']) && !title.contains(['\u{1b}', '\u{7}']));
        assert_eq!(removed, 4);
    }
}
