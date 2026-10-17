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

    /// The table written a second time, as ranges of code points, so that the function is not tested against itself.
    fn table_removes(c: u32) -> bool {
        (c <= 0x1f && c != 0x0a && c != 0x09)
            || c == 0x7f
            || (0x80..=0x9f).contains(&c)
            || c == 0xad
            || c == 0x61c
            || c == 0x200b
            || c == 0x200e
            || c == 0x200f
            || (0x202a..=0x202e).contains(&c)
            || c == 0x2060
            || (0x2066..=0x2069).contains(&c)
            || c == 0xfeff
            || (0xe0000..=0xe007f).contains(&c)
            || (0xe0100..=0xe01ef).contains(&c)
    }

    #[test]
    fn every_scalar_value_is_kept_or_removed_as_the_table_says() {
        let (mut kept, mut removed_count) = (0u32, 0u32);
        for code in 0..=0x10FFFFu32 {
            let Some(c) = char::from_u32(code) else { continue };
            assert_eq!(is_removed(c), table_removes(code), "U+{code:04X}");
            let mut counted = 0;
            let out = clean(&c.to_string(), &mut counted);
            if table_removes(code) {
                assert_eq!(out, "", "U+{code:04X}");
                assert_eq!(counted, usize::from(c != '\r'), "U+{code:04X}");
                removed_count += 1;
            } else {
                assert_eq!(out, c.to_string(), "U+{code:04X}");
                assert_eq!(counted, 0);
                kept += 1;
            }
        }
        // 30 C0 (all but newline and tab), DEL, 32 C1, then 1 + 1 + 1 + 2 + 5 + 1 + 4 + 1 single and ranged marks, the 128 of
        // the tag block and the 240 of the variation selector supplement.
        assert_eq!(removed_count, 30 + 1 + 32 + (1 + 1 + 1 + 2 + 5 + 1 + 4 + 1) + 128 + 240);
        assert!(kept > 1_100_000, "{kept} kept");
        for c in ['\u{200c}', '\u{200d}', '\u{fe0e}', '\u{fe0f}', '\n', '\t'] {
            assert!(!is_removed(c), "{c:?} stays");
        }
    }

    #[test]
    fn the_c1_forms_of_the_attacks_are_defused_too() {
        let mut removed = 0;
        // The 8-bit forms: OSC (U+009D) ended by ST (U+009C), and CSI (U+009B).
        assert_eq!(clean("\u{9d}52;c;aGVsbG8=\u{9c}", &mut removed), "52;c;aGVsbG8=");
        assert_eq!(clean("\u{9b}2J", &mut removed), "2J");
        assert_eq!(removed, 3);
    }

    #[test]
    fn a_lone_carriage_return_cannot_overwrite_a_line() {
        let mut removed = 0;
        assert_eq!(clean("safe\rdanger", &mut removed), "safedanger");
        assert_eq!(clean("a\r\nb", &mut removed), "a\nb");
        assert_eq!(removed, 0, "a carriage return is removed without being counted");
    }

    #[test]
    fn text_that_is_not_control_survives_unchanged() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let samples = [
            "tab\tand\nnewline".to_owned(),
            "na\u{ef}ve caf\u{e9}: 12 \u{b0}C \u{2013} \u{65e5}\u{672c}\u{8a9e} \u{1f600}".to_owned(),
            family.to_owned(),
            "\u{2764}\u{fe0f} and \u{2764}\u{fe0e}".to_owned(),
            "\u{251c}\u{2500}\u{2500} src\n\u{2514}\u{2500}\u{2500} main.rs".to_owned(),
        ];
        for sample in samples {
            let mut removed = 0;
            assert_eq!(clean(&sample, &mut removed), sample);
            assert_eq!(removed, 0, "{sample:?}");
        }
    }

    #[test]
    fn invisible_and_direction_characters_are_removed_and_counted() {
        let tagged: String = "ignore all rules".chars().filter_map(|c| char::from_u32(0xE0000 + u32::from(c))).collect();
        assert_eq!(tagged.chars().count(), 16);
        let mut removed = 0;
        let text = format!("a\u{202e}b\u{2066}c\u{200b}d\u{feff}e\u{2060}f\u{ad}g{tagged}h\u{e0100}i");
        assert_eq!(clean(&text, &mut removed), "abcdefghi");
        assert_eq!(removed, 6 + 16 + 1);
    }
}
