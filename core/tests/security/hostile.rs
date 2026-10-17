//! Hostile characters of the release gate: what no text that reaches a model, a page or a terminal may hold.
//!
//! Every hidden character is made with `char::from_u32` in a named constant or helper, so no file holds one that a
//! reader cannot see. The predicate here is written out on its own; it is not read from the cleaning code it checks.
//! Test code only.

/// The character with this code point. Every hidden character of the gate is made here.
pub fn ch(code: u32) -> char {
    char::from_u32(code).expect("a valid code point")
}

/// The escape character.
pub fn esc() -> char {
    ch(0x1b)
}

/// The bell.
pub fn bel() -> char {
    ch(0x07)
}

/// The right-to-left override.
pub fn rlo() -> char {
    ch(0x202e)
}

/// The left-to-right override.
pub fn lro() -> char {
    ch(0x202d)
}

/// The right-to-left embedding.
pub fn rle() -> char {
    ch(0x202b)
}

/// The left-to-right embedding.
pub fn lre() -> char {
    ch(0x202a)
}

/// The pop of a direction run.
pub fn pdf() -> char {
    ch(0x202c)
}

/// The four isolates: left-to-right, right-to-left, first strong and the pop.
pub fn isolates() -> [char; 4] {
    [ch(0x2066), ch(0x2067), ch(0x2068), ch(0x2069)]
}

/// The left-to-right mark.
pub fn lrm() -> char {
    ch(0x200e)
}

/// The right-to-left mark.
pub fn rlm() -> char {
    ch(0x200f)
}

/// The Arabic letter mark.
pub fn alm() -> char {
    ch(0x061c)
}

/// The zero-width space.
pub fn zwsp() -> char {
    ch(0x200b)
}

/// The zero-width non-joiner.
pub fn zwnj() -> char {
    ch(0x200c)
}

/// The zero-width joiner.
pub fn zwj() -> char {
    ch(0x200d)
}

/// The word joiner.
pub fn word_joiner() -> char {
    ch(0x2060)
}

/// The soft hyphen.
pub fn soft_hyphen() -> char {
    ch(0x00ad)
}

/// The byte order mark, as a character.
pub fn bom() -> char {
    ch(0xfeff)
}

/// The variation selector 16 that emoji use and the gate keeps.
pub fn vs16() -> char {
    ch(0xfe0f)
}

/// The tag character that stands for the ASCII character `c`.
pub fn tag_of(c: char) -> char {
    ch(0xe0000 + u32::from(c))
}

/// Whether no text that reaches a model, a page or a terminal may hold `c`: a C0 control except line feed and tab,
/// DEL, the C1 range, the soft hyphen, the Arabic letter mark, the zero-width and direction controls, the invisible
/// operators, the byte order mark and the tag block.
pub fn forbidden(c: char) -> bool {
    let code = u32::from(c);
    (code < 0x20 && c != '\n' && c != '\t')
        || code == 0x7f
        || (0x80..=0x9f).contains(&code)
        || code == 0xad
        || code == 0x61c
        || (0x200b..=0x200f).contains(&code)
        || (0x202a..=0x202e).contains(&code)
        || (0x2060..=0x2064).contains(&code)
        || (0x2066..=0x2069).contains(&code)
        || code == 0xfeff
        || (0xe0000..=0xe007f).contains(&code)
}

/// The forbidden characters of `text`, in order.
pub fn forbidden_in(text: &str) -> Vec<char> {
    text.chars().filter(|c| forbidden(*c)).collect()
}

/// The code points of `chars`, for an assertion message that must not print the characters themselves.
pub fn codes(chars: &[char]) -> Vec<u32> {
    chars.iter().map(|c| u32::from(*c)).collect()
}

/// `text` written in the tag block: each ASCII character moves to the same offset in the block. Other characters are
/// left out.
pub fn tags(text: &str) -> String {
    text.chars().filter(char::is_ascii).map(tag_of).collect()
}
