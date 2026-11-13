//! Bracketed pastes in the input of a session, kept whole.
//!
//! A program that asked for bracketed paste reads a paste as `ESC [ 200 ~`, the text, `ESC [ 201 ~`. When the program leaves
//! too much of its input unread, the session host drops input ([`holzwave_core::pane::MAX_RAW_BACKLOG`]); dropped frame by
//! frame, a paste could lose its end marker and leave the program in paste mode, or reach it without its start. So the input
//! of one client goes through a [`PasteGate`]: a paste is held until its end marker has arrived and is then handed on as one
//! piece, which the pane takes whole or refuses whole; a paste that cannot fit in the room the program has left is dropped
//! whole while it arrives.
//!
//! The gate relies on how a client sends input: one write of the pane is cut into frames of the full size from its start, so
//! a paste starts at the start of its write and only the last frame of the write is shorter. A start marker that is cut
//! between two frames is therefore not seen as a paste (no client makes one), and a write that ends, with a frame shorter than
//! the full size, before the end marker came was not a paste: what was held is handed on as it is. So a person who types the
//! six bytes of a start marker by hand never holds back their own input.

/// What a program that asked for bracketed paste reads before a paste.
pub const PASTE_START: &[u8] = b"\x1b[200~";

/// What it reads after the paste.
pub const PASTE_END: &[u8] = b"\x1b[201~";

/// One piece of input for the program, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Input that is not a paste.
    Bytes(Vec<u8>),
    /// A whole paste, both markers included: it reaches the program whole or not at all.
    Paste(Vec<u8>),
}

impl Part {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Part::Bytes(bytes) | Part::Paste(bytes) => bytes,
        }
    }
}

/// What one frame gives: the pieces to write, in order, and whether a paste was dropped.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Fed {
    pub parts: Vec<Part>,
    pub dropped: bool,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Outside,
    /// A paste has started; everything of it so far, the start marker first.
    Inside(Vec<u8>),
    /// A paste that did not fit is being dropped until its end; the last bytes, which may hold the first part of the end
    /// marker.
    Dropping(Vec<u8>),
}

/// The pastes of one client in one session.
#[derive(Debug, Default)]
pub struct PasteGate {
    state: State,
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// The last bytes of `bytes` that can be the first part of an end marker.
fn tail(bytes: &[u8]) -> Vec<u8> {
    bytes[bytes.len().saturating_sub(PASTE_END.len() - 1)..].to_vec()
}

impl PasteGate {
    /// Takes one frame of input. `full` is the size of a frame that a longer write was cut into; `room` is how many bytes may
    /// still wait for the program, so a paste that grows past it cannot be written and is dropped at once.
    pub fn feed(&mut self, frame: &[u8], full: usize, room: usize) -> Fed {
        let more_follows = frame.len() >= full;
        let mut fed = Fed::default();
        let mut rest = frame.to_vec();
        loop {
            match std::mem::take(&mut self.state) {
                State::Outside => match find(&rest, PASTE_START) {
                    None => {
                        if !rest.is_empty() {
                            fed.parts.push(Part::Bytes(rest));
                        }
                        return fed;
                    }
                    Some(at) => {
                        let paste = rest.split_off(at);
                        if !rest.is_empty() {
                            fed.parts.push(Part::Bytes(rest));
                        }
                        self.state = State::Inside(Vec::new());
                        rest = paste;
                    }
                },
                State::Inside(mut held) => {
                    let from = held.len().saturating_sub(PASTE_END.len() - 1);
                    held.extend_from_slice(&rest);
                    if let Some(at) = find(&held[from..], PASTE_END) {
                        rest = held.split_off(from + at + PASTE_END.len());
                        if held.len() > room {
                            fed.dropped = true;
                        } else {
                            fed.parts.push(Part::Paste(held));
                        }
                        continue;
                    }
                    if held.len() > room {
                        fed.dropped = true;
                        if more_follows {
                            self.state = State::Dropping(tail(&held));
                        }
                    } else if more_follows {
                        self.state = State::Inside(held);
                    } else {
                        // The write ended without the end marker: it was no paste, and what was held is input as it is.
                        fed.parts.push(Part::Bytes(held));
                    }
                    return fed;
                }
                State::Dropping(mut seen) => {
                    seen.extend_from_slice(&rest);
                    if let Some(at) = find(&seen, PASTE_END) {
                        rest = seen.split_off(at + PASTE_END.len());
                        continue;
                    }
                    if more_follows {
                        self.state = State::Dropping(tail(&seen));
                    }
                    return fed;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: usize = 16;
    const ROOM: usize = 1 << 20;

    fn paste(text: &[u8]) -> Vec<u8> {
        [PASTE_START, text, PASTE_END].concat()
    }

    /// Cuts one write into frames as a client does: full frames from its start, a shorter last one.
    fn frames(write: &[u8]) -> Vec<&[u8]> {
        write.chunks(FULL).collect()
    }

    fn feed_all(gate: &mut PasteGate, write: &[u8], room: usize) -> Fed {
        let mut all = Fed::default();
        for frame in frames(write) {
            let fed = gate.feed(frame, FULL, room);
            all.parts.extend(fed.parts);
            all.dropped |= fed.dropped;
        }
        all
    }

    #[test]
    fn input_that_is_no_paste_passes_as_it_is() {
        let mut gate = PasteGate::default();
        assert_eq!(gate.feed(b"ls\r", FULL, ROOM).parts, [Part::Bytes(b"ls\r".to_vec())]);
        assert_eq!(gate.feed(b"\x1b", FULL, ROOM).parts, [Part::Bytes(b"\x1b".to_vec())], "a lone escape is not held");
        assert_eq!(gate.feed(b"", FULL, ROOM), Fed::default());
    }

    #[test]
    fn a_paste_is_handed_on_whole_once_its_end_arrived_with_what_came_before_and_after_it() {
        let mut gate = PasteGate::default();
        let fed = gate.feed(&[b"a".as_slice(), &paste(b"xy"), b"b"].concat(), 64, ROOM);
        assert_eq!(fed.parts, [Part::Bytes(b"a".to_vec()), Part::Paste(paste(b"xy")), Part::Bytes(b"b".to_vec())]);
        assert!(!fed.dropped);
        // Two pastes in one write.
        let fed = gate.feed(&[paste(b"1"), paste(b"2")].concat(), 64, ROOM);
        assert_eq!(fed.parts, [Part::Paste(paste(b"1")), Part::Paste(paste(b"2"))]);
    }

    #[test]
    fn a_paste_over_many_frames_with_its_end_marker_cut_between_two_is_held_and_handed_on_as_one_piece() {
        for len in 0..3 * FULL {
            let mut gate = PasteGate::default();
            let text = vec![b'p'; len];
            let fed = feed_all(&mut gate, &paste(&text), ROOM);
            assert_eq!(fed.parts, [Part::Paste(paste(&text))], "a paste of {len}");
            let tight = feed_all(&mut PasteGate::default(), &paste(&text), paste(&text).len());
            assert_eq!(tight.parts, [Part::Paste(paste(&text))], "a paste of {len} that just fits");
            assert!(!fed.dropped);
            assert_eq!(gate.feed(b"z", FULL, ROOM).parts, [Part::Bytes(b"z".to_vec())], "and the gate is open again");
        }
    }

    #[test]
    fn a_paste_that_does_not_fit_is_dropped_whole_and_input_after_its_end_passes() {
        let room = 2 * FULL;
        for len in room..room + 3 * FULL {
            let mut gate = PasteGate::default();
            let write = [paste(&vec![b'p'; len]), b"after".to_vec()].concat();
            let fed = feed_all(&mut gate, &write, room);
            assert!(fed.dropped, "a paste of {len}");
            assert!(fed.parts.iter().all(|part| matches!(part, Part::Bytes(_))), "a paste of {len}: {:?}", fed.parts);
            let written: Vec<u8> = fed.parts.iter().flat_map(|part| part.bytes().to_vec()).collect();
            assert_eq!(written, b"after", "a paste of {len}: no marker and no byte of it is written");
        }
    }

    #[test]
    fn a_write_that_ends_without_the_end_marker_was_no_paste_and_is_input_as_it_is() {
        let mut gate = PasteGate::default();
        // A person typing the six bytes of the start marker: each keystroke is a write of its own.
        let mut parts = Vec::new();
        for byte in PASTE_START.iter().chain(b"ls\r") {
            parts.extend(gate.feed(&[*byte], FULL, ROOM).parts);
        }
        let typed: Vec<u8> = parts.iter().flat_map(|p| p.bytes().to_vec()).collect();
        assert_eq!(typed, [PASTE_START, b"ls\r"].concat());
        // A start marker in a short write with no end: the write is handed on as it is, and the next one passes.
        let fed = gate.feed(&[PASTE_START, b"x"].concat(), FULL, ROOM);
        assert_eq!(fed.parts, [Part::Bytes([PASTE_START, b"x"].concat())]);
        assert_eq!(gate.feed(b"y", FULL, ROOM).parts, [Part::Bytes(b"y".to_vec())]);
    }
}
