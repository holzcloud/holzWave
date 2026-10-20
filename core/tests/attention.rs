//! The flood gate of the attention events (docs/design.md section 6.23): a pane lets one bell through per half second,
//! whatever its program writes. Synthetic instants only, nothing sleeps.

use std::time::{Duration, Instant};

use holzwave_core::attention::{CommandMarks, GAP, Gate};

#[test]
fn the_gap_is_half_a_second() {
    assert_eq!(GAP, Duration::from_millis(500));
}

#[test]
fn the_first_call_passes() {
    let mut gate = Gate::new();
    assert!(gate.pass(Instant::now()));
}

#[test]
fn a_call_inside_the_gap_does_not_pass_and_one_at_the_gap_does() {
    let t0 = Instant::now();
    let mut gate = Gate::new();
    assert!(gate.pass(t0));
    assert!(!gate.pass(t0));
    assert!(!gate.pass(t0 + Duration::from_millis(499)));
    assert!(gate.pass(t0 + GAP));
}

#[test]
fn the_reference_is_the_last_pass_not_the_last_attempt() {
    let t0 = Instant::now();
    let mut gate = Gate::new();
    assert!(gate.pass(t0));
    // Attempts every 100 ms keep failing until 500 ms after the pass; they do not push the window out.
    for ms in [100, 200, 300, 400] {
        assert!(!gate.pass(t0 + Duration::from_millis(ms)), "{ms} ms");
    }
    assert!(gate.pass(t0 + Duration::from_millis(500)));
    assert!(!gate.pass(t0 + Duration::from_millis(600)));
    assert!(gate.pass(t0 + Duration::from_millis(1000)));
}

#[test]
fn a_flood_over_one_second_passes_at_most_two() {
    let t0 = Instant::now();
    let mut gate = Gate::new();
    let passed = (0..1000).filter(|i| gate.pass(t0 + Duration::from_micros(i * 1000))).count();
    assert_eq!(passed, 2, "the passes land at 0 ms and 500 ms");
}

#[test]
fn an_earlier_instant_neither_panics_nor_passes() {
    let t0 = Instant::now() + Duration::from_secs(10);
    let mut gate = Gate::new();
    assert!(gate.pass(t0));
    assert!(!gate.pass(t0 - Duration::from_secs(5)));
    assert!(!gate.pass(t0 - GAP));
}

// ---- CommandMarks: the OSC 133 D scanner (docs/design.md section 6.23, T-eog-03) ----

/// Run the scanner over `chunks` (each fed as one call) and collect what it reports.
fn marks_of(chunks: &[&[u8]]) -> Vec<Option<u32>> {
    let mut scanner = CommandMarks::new();
    let mut out = Vec::new();
    for chunk in chunks {
        scanner.feed(chunk, |status| out.push(status));
    }
    out
}

fn marks(bytes: &[u8]) -> Vec<Option<u32>> {
    marks_of(&[bytes])
}

#[test]
fn a_d_mark_reports_its_status() {
    assert_eq!(marks(b"\x1b]133;D;0\x1b\\"), vec![Some(0)]);
    assert_eq!(marks(b"\x1b]133;D;0\x07"), vec![Some(0)], "BEL terminator");
    assert_eq!(marks(b"\x1b]133;D\x1b\\"), vec![None], "no status");
    assert_eq!(marks(b"\x1b]133;D\x07"), vec![None]);
    assert_eq!(marks(b"\x1b]133;D;130\x1b\\"), vec![Some(130)]);
    assert_eq!(marks(b"\x1b]133;D;2;aid=7\x1b\\"), vec![Some(2)], "status, then options");
    assert_eq!(marks(b"\x1b]133;D;4294967295\x07"), vec![Some(u32::MAX)]);
    assert_eq!(marks(b"\x1b]133;D;0000000003\x07"), vec![Some(3)], "ten digits with leading zeros");
}

#[test]
fn other_sequences_and_malformed_marks_report_nothing() {
    for bad in [
        &b"\x1b]133;A\x07"[..],
        b"\x1b]133;B\x07",
        b"\x1b]133;C\x07",
        b"\x1b]133;DX\x07",
        b"\x1b]133;D;abc\x07",
        b"\x1b]133;D;\x07",
        b"\x1b]133;D;-1\x07",
        b"\x1b]133;D;1x\x07",
        b"\x1b]13;D\x07",
        b"\x1b]1333;D\x07",
        b"\x1b]133;D;99999999999\x07",
        b"\x1b]133;D;4294967296\x07",
        b"\x1b]2;title\x07",
        b"133;D;1",
        b"plain text 133;D\r\n",
        b"\x9d133;D;1\x9c",
    ] {
        assert_eq!(marks(bad), Vec::<Option<u32>>::new(), "{:?}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn an_overlong_payload_is_dropped_and_does_not_swallow_the_next_mark() {
    let mut long = b"\x1b]133;D;1;".to_vec();
    long.extend(std::iter::repeat_n(b'a', 100));
    long.push(0x07);
    assert_eq!(marks(&long), Vec::<Option<u32>>::new());

    long.extend_from_slice(b"\x1b]133;D;5\x1b\\");
    assert_eq!(marks(&long), vec![Some(5)]);

    // ST-terminated overlong payload, then a valid mark.
    let mut st = b"\x1b]133;D;1;".to_vec();
    st.extend(std::iter::repeat_n(b'b', 5000));
    st.extend_from_slice(b"\x1b\\\x1b]133;D;6\x07");
    assert_eq!(marks(&st), vec![Some(6)]);

    // The longest payload that still counts is 24 bytes: `133;D;` + options up to 24.
    let at_limit = b"\x1b]133;D;7;aid=12345678\x07"; // payload "133;D;7;aid=12345678" is 20 bytes
    assert_eq!(marks(at_limit), vec![Some(7)]);
    let over = b"\x1b]133;D;7;aid=1234567890123\x07"; // 25+ bytes
    assert_eq!(marks(over), Vec::<Option<u32>>::new());
}

#[test]
fn can_and_sub_cancel_and_a_new_escape_starts_over() {
    assert_eq!(marks(b"\x1b]133;D;1\x18\x07"), Vec::<Option<u32>>::new(), "CAN cancels, the BEL is then plain");
    assert_eq!(marks(b"\x1b]133;D;1\x1a\x07"), Vec::<Option<u32>>::new(), "SUB cancels");
    assert_eq!(marks(b"\x1b]133;D;1\x18\x1b]133;D;2\x07"), vec![Some(2)]);
    // ESC ] inside a sequence starts a new one; the first was never finished.
    assert_eq!(marks(b"\x1b]133;D;1\x1b]133;D;2\x07"), vec![Some(2)]);
    // ESC followed by something else ends the sequence without a mark.
    assert_eq!(marks(b"\x1b]133;D;1\x1bX\x07"), Vec::<Option<u32>>::new());
    // Two marks back to back.
    assert_eq!(marks(b"\x1b]133;D;1\x07\x1b]133;D;2\x1b\\"), vec![Some(1), Some(2)]);
    // Text around a mark does not matter.
    assert_eq!(marks(b"out\r\n\x1b]133;D;9\x07\x1b]133;A\x07$ "), vec![Some(9)]);
}

#[test]
fn bytes_fed_one_at_a_time_give_the_same_marks() {
    let stream: &[u8] = b"a\x1b]133;D;0\x07b\x1b]133;D\x1b\\\x1b]133;D;130;aid=1\x1b\\c\x1b]2;t\x07\x1b]133;D;x\x07\x1b]133;D;4\x07";
    let whole = marks(stream);
    assert_eq!(whole, vec![Some(0), None, Some(130), Some(4)]);
    let single: Vec<&[u8]> = stream.chunks(1).collect();
    assert_eq!(marks_of(&single), whole);
    let pairs: Vec<&[u8]> = stream.chunks(3).collect();
    assert_eq!(marks_of(&pairs), whole);
}

/// A xorshift generator, so the stream is the same on every run and no dependency is needed.
struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[test]
fn a_pseudo_random_megabyte_with_planted_marks_yields_exactly_the_planted_marks() {
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let mut stream = Vec::with_capacity(1 << 20);
    let mut planted = Vec::new();
    while stream.len() < (1 << 20) {
        // Noise without ESC, CAN and SUB, so it can neither start nor break a mark and cannot form one.
        let run = (rng.next() % 600) as usize;
        for _ in 0..run {
            let b = (rng.next() & 0xff) as u8;
            stream.push(if matches!(b, 0x1b | 0x18 | 0x1a) { b'.' } else { b });
        }
        let status = (rng.next() % 300) as u32;
        let with_status = rng.next() % 4 != 0;
        let bel = rng.next() % 2 == 0;
        stream.extend_from_slice(b"\x1b]133;D");
        if with_status {
            stream.extend_from_slice(format!(";{status}").as_bytes());
            planted.push(Some(status));
        } else {
            planted.push(None);
        }
        stream.extend_from_slice(if bel { b"\x07" } else { b"\x1b\\" });
    }
    assert_eq!(marks(&stream), planted);

    // Pure noise, ESC included, never panics and never reports a status that was not planted: random bytes
    // cannot contain `ESC ] 133 ; D` by chance in this stream (checked, then ignored).
    let mut noise = Vec::with_capacity(1 << 20);
    for _ in 0..(1 << 20) {
        noise.push((rng.next() & 0xff) as u8);
    }
    let found = marks(&noise);
    assert!(found.len() < 4, "{} marks in noise", found.len());
}
