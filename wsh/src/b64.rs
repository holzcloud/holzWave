//! Strict standard base64, for the arguments that the PowerShell hook sends to `wsh hist add --b64`.
//!
//! Windows PowerShell 5.1 and PowerShell 7 before 7.3 do not escape a double quote inside an argument for a native program,
//! so a typed command that holds quotes would arrive cut or split. The hook therefore passes the command and the directory
//! as base64 text, which holds only letters, digits, `+`, `/` and `=`, and no quoting rule can change it. `wsh` decodes it
//! here: only the standard alphabet, only canonical padding, no whitespace, and the size is checked before anything is
//! decoded. std only; nothing here can panic and nothing is printed.

/// The value of one symbol of the standard alphabet.
fn symbol(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some(u32::from(byte - b'A')),
        b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
        b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// The bytes that `text` encodes, or `None` when it is not canonical standard base64: the length is a multiple of four,
/// `=` appears only as one or two padding characters at the very end, nothing but the 64 symbols stands before them, and the
/// unused bits of the last symbol are zero. The empty text decodes to no bytes.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let pad = bytes.iter().rev().take_while(|b| **b == b'=').count();
    if pad > 2 {
        return None;
    }
    let body = &bytes[..bytes.len() - pad];
    let mut out = Vec::with_capacity((bytes.len() / 4 * 3).saturating_sub(pad));
    let (mut acc, mut bits) = (0u32, 0u32);
    for &byte in body {
        acc = acc << 6 | symbol(byte)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // What is left are the unused bits of the last symbol: canonical text has them all zero.
    (acc == 0).then_some(out)
}

/// The text that `text` encodes, at most `max_bytes` long: `None` when the encoded text is longer than the encoding of
/// `max_bytes` bytes (checked before anything is decoded), when it is not canonical base64, when the decoded bytes are longer
/// than `max_bytes` or when they are not valid UTF-8.
pub fn decode_text(text: &str, max_bytes: usize) -> Option<String> {
    if text.len() > (max_bytes / 3).saturating_mul(4).saturating_add(4) {
        return None;
    }
    let bytes = decode(text)?;
    if bytes.len() > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rfc_4648_vectors_decode() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(decode(encoded).as_deref(), Some(plain.as_bytes()), "{encoded}");
        }
    }

    #[test]
    fn real_commands_and_directories_survive() {
        let cases = [
            ("ZWNobyAicXVvdGVkIHdvcmRzIg==", "echo \"quoted words\""),
            ("Qzpc", "C:\\"),
            ("bmHDr3ZlIGNhZsOpIOaXpeacrOiqniDwn5iA", "naïve café 日本語 😀"),
        ];
        for (encoded, plain) in cases {
            assert_eq!(decode_text(encoded, 8192).as_deref(), Some(plain), "{encoded}");
        }
    }

    #[test]
    fn everything_that_is_not_canonical_standard_base64_is_refused() {
        for bad in ["Zg", "Zg=", "Z===", "Zm9v\n", "Zm9v Zm9v", "Zm-v", "Zm_v", "Zh==", "Zm9=", "====", "Zg==Zg==", "=Zg=", "Zm9v=", " Zm9v", "Zm9\u{e4}"] {
            assert_eq!(decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn bytes_that_are_not_text_decode_but_are_not_text() {
        assert_eq!(decode("/w==").as_deref(), Some(&[0xff][..]));
        assert_eq!(decode_text("/w==", 100), None);
    }

    #[test]
    fn the_size_is_checked_before_the_text_is_decoded() {
        // The cap for three bytes is eight symbols: nine are refused although nine is not even valid base64.
        assert_eq!(decode_text("Zm9vZm9vZ", 3), None);
        // A valid text within the symbol cap but with too many bytes is refused after decoding.
        assert_eq!(decode_text("Zm9vZm9v", 3), None);
        assert_eq!(decode_text("Zm9v", 3).as_deref(), Some("foo"));
        assert_eq!(decode_text("", 0).as_deref(), Some(""));
        assert_eq!(decode_text("Zg==", 0), None);
        assert_eq!(decode_text("AAAA", usize::MAX), Some("\0\0\0".to_owned()), "a huge cap does not overflow");
    }

    #[test]
    fn every_symbol_and_length_round_trips() {
        let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        // Every symbol of the alphabet, four at a time.
        for quad in alphabet.as_bytes().chunks(4) {
            let text = std::str::from_utf8(quad).unwrap_or_default();
            assert_eq!(decode(text).map(|b| b.len()), Some(3), "{text}");
        }
        for len in 0..40usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let mut text = String::new();
            for chunk in data.chunks(3) {
                let bits = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
                for i in 0..4 {
                    if i <= chunk.len() {
                        text.push(char::from(alphabet.as_bytes()[((bits >> (18 - 6 * i)) & 0x3f) as usize]));
                    } else {
                        text.push('=');
                    }
                }
            }
            assert_eq!(decode(&text), Some(data), "{len}: {text}");
        }
    }
}
