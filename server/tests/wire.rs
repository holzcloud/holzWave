//! The frames between the supervisor and a worker: the layout pinned byte for byte, round trips, truncation and garbage.
//!
//! The layout is written in the doc comment of `server/src/wire.rs`; every pin below is the literal bytes of one frame.

#[path = "support/scratch.rs"]
mod scratch;
#[path = "support/worker_env.rs"]
mod worker_env;

use std::io::{self, Cursor, Read};

use holzwave_server::wire::{
    CmdFrame, Ctl, CtlOutcome, Evt, ExitReason, FILE_PIECE, FILE_SOCKET, FileWhat, Frame, Key, MAX_PAYLOAD, Note,
    OpeningKey, PROTOCOL_VERSION, Res, ResBody, SlotKey, StoreState, WireError, codec, read_frame, write_frame,
};
use holzwave_server::worker::secretstore::SlotKind;
use zeroize::{ZeroizeOnDrop, Zeroizing};
use tokio_util::bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

/// The payload of `frame`, without the length prefix.
fn payload(frame: &Frame) -> Vec<u8> {
    frame.to_payload().expect("the frame encodes").to_vec()
}

/// The same bytes as they travel: the length prefix and the payload.
fn on_the_wire(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::new();
    write_frame(&mut out, frame).expect("the frame is written");
    out
}

fn prefixed(payload: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(payload.len()).expect("a short pin").to_be_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}

#[test]
fn tracer_a_command_is_kind_1_then_socket_request_and_the_json_text() {
    let json = r#"{"cmd":"get_layout","args":{}}"#;
    let frame = Frame::Cmd(CmdFrame { socket: 7, request: 1, json: json.to_owned() });
    let mut expected = vec![0x01, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 1];
    expected.extend_from_slice(json.as_bytes());
    assert_eq!(payload(&frame), expected);
    assert_eq!(on_the_wire(&frame), prefixed(&expected));
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), frame);
}

#[test]
fn tracer_an_answer_is_kind_2_then_socket_request_status_and_the_body() {
    let text = Frame::Res(Res { socket: 0x0102_0304, request: 0x0A0B_0C0D_0E0F_1011, body: ResBody::Ok("[1,2]".into()) });
    let mut expected = vec![0x02, 1, 2, 3, 4, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0];
    expected.extend_from_slice(b"[1,2]");
    assert_eq!(payload(&text), expected);
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), text);

    let error = Frame::Res(Res { socket: 9, request: 2, body: ResBody::Err("\"no such pane\"".into()) });
    let mut expected = vec![0x02, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 2, 1];
    expected.extend_from_slice(b"\"no such pane\"");
    assert_eq!(payload(&error), expected);
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), error);

    let binary = Frame::Res(Res { socket: 9, request: 3, body: ResBody::Binary(vec![0xFF, 0x00, 0xFE]) });
    assert_eq!(payload(&binary), vec![0x02, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 3, 2, 0xFF, 0x00, 0xFE]);
    assert_eq!(Frame::from_payload(&payload(&binary)).expect("decodes"), binary);
}

#[test]
fn tracer_an_event_is_kind_3_then_target_name_length_name_and_the_final_text() {
    let to_all = Frame::Evt(Evt { target: 0, name: "pane-exit".into(), text: r#"{"pane":3,"code":null}"#.into() });
    let mut expected = vec![0x03, 0, 0, 0, 0, 9];
    expected.extend_from_slice(b"pane-exit");
    expected.extend_from_slice(br#"{"pane":3,"code":null}"#);
    assert_eq!(payload(&to_all), expected);
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), to_all);

    let to_one = Frame::Evt(Evt { target: 9, name: "title".into(), text: r#"{"pane":1,"title":"x"}"#.into() });
    let mut expected = vec![0x03, 0, 0, 0, 9, 5];
    expected.extend_from_slice(b"title");
    expected.extend_from_slice(br#"{"pane":1,"title":"x"}"#);
    assert_eq!(payload(&to_one), expected);
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), to_one);
}

#[test]
fn tracer_init_is_kind_4_operation_1_then_version_and_the_three_caps() {
    // Version 2: the bytes of version 1 with the new version and four more bytes at the end, the picture cap in MiB.
    let frame = Frame::Ctl(Ctl::Init { version: 2, max_panes: 3, max_scrollback_lines: 0x0001_0000, max_image_mib: 0x0000_0040 });
    let expected = vec![0x04, 0x01, 0, 2, 0, 0, 0, 3, 0, 1, 0, 0, 0, 0, 0, 0x40];
    assert_eq!(payload(&frame), expected);
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), frame);
}

/// The first control frame as the supervisor wrote it before the picture cap existed: version 1, two caps, twelve bytes.
const OLD_INIT: [u8; 12] = [0x04, 0x01, 0, 1, 0, 0, 0, 3, 0, 1, 0, 0];

#[test]
fn init_the_picture_cap_is_the_last_four_bytes_in_big_endian_and_the_edges_are_pinned() {
    // Written out by hand: kind, operation, version 2, max_panes 0, max_scrollback_lines 0, then 4096 = 0x00001000.
    let frame = Frame::Ctl(Ctl::Init { version: 2, max_panes: 0, max_scrollback_lines: 0, max_image_mib: 4096 });
    let expected = vec![0x04, 0x01, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10, 0];
    assert_eq!(payload(&frame), expected);
    assert_eq!(on_the_wire(&frame), prefixed(&expected));
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), frame);

    // Zero is a value, not an absence: the worker reads it as "no pictures".
    let off = Frame::Ctl(Ctl::Init { version: 2, max_panes: 64, max_scrollback_lines: 10_000, max_image_mib: 0 });
    let expected = vec![0x04, 0x01, 0, 2, 0, 0, 0, 64, 0, 0, 0x27, 0x10, 0, 0, 0, 0];
    assert_eq!(payload(&off), expected);
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), off);

    // The frame carries what it is given; the range of the key is the configuration's business.
    let widest = Frame::Ctl(Ctl::Init { version: 2, max_panes: 1, max_scrollback_lines: 100, max_image_mib: u32::MAX });
    let expected = vec![0x04, 0x01, 0, 2, 0, 0, 0, 1, 0, 0, 0, 100, 0xFF, 0xFF, 0xFF, 0xFF];
    assert_eq!(payload(&widest), expected);
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), widest);
}

#[test]
fn init_the_protocol_version_is_2() {
    assert_eq!(PROTOCOL_VERSION, 2, "the first frame has a field that version 1 did not");
}

#[test]
fn init_a_frame_of_the_old_length_is_cut_off_and_one_byte_more_than_the_new_length_is_trailing() {
    // The twelve bytes of version 1 end four bytes before the picture cap: the decoder says so and does not guess a cap.
    assert!(matches!(Frame::from_payload(&OLD_INIT), Err(WireError::Truncated)));
    let mut longer = vec![0x04, 0x01, 0, 2, 0, 0, 0, 3, 0, 1, 0, 0, 0, 0, 0, 0x40];
    assert!(Frame::from_payload(&longer).is_ok());
    longer.push(0);
    assert!(matches!(Frame::from_payload(&longer), Err(WireError::Trailing)));
    // A frame cut anywhere inside the new four bytes is cut off as well.
    for cut in 12..16 {
        assert!(matches!(Frame::from_payload(&longer[..cut]), Err(WireError::Truncated)), "cut at {cut}");
    }
}

#[test]
fn tracer_attach_is_kind_4_operation_2_then_the_socket() {
    let frame = Frame::Ctl(Ctl::Attach { socket: 9 });
    let expected = vec![0x04, 0x02, 0, 0, 0, 9];
    assert_eq!(payload(&frame), expected);
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), frame);
}

#[test]
fn tracer_ready_is_kind_5_operation_1_and_nothing_else() {
    let frame = Frame::Note(Note::Ready);
    assert_eq!(payload(&frame), vec![0x05, 0x01]);
    assert_eq!(Frame::from_payload(&[0x05, 0x01]).expect("decodes"), frame);
}

#[test]
fn tracer_frames_read_back_one_after_another_and_the_end_between_two_is_none() {
    let frames = vec![
        Frame::Ctl(Ctl::Init { version: 2, max_panes: 0, max_scrollback_lines: 0, max_image_mib: 0 }),
        Frame::Cmd(CmdFrame { socket: 1, request: 1, json: "{}".into() }),
        Frame::Res(Res { socket: 1, request: 1, body: ResBody::Ok("null".into()) }),
        Frame::Evt(Evt { target: 0, name: "frame".into(), text: "{}".into() }),
        Frame::Note(Note::Ready),
    ];
    let mut bytes = Vec::new();
    for frame in &frames {
        write_frame(&mut bytes, frame).expect("written");
    }
    let mut reader = Cursor::new(bytes);
    for frame in &frames {
        assert_eq!(&read_frame(&mut reader).expect("read").expect("a frame"), frame);
    }
    assert!(read_frame(&mut reader).expect("a clean end").is_none());
}

/// A reader that hands out the four bytes of a length prefix and fails on any further read, so a frame that is refused
/// before its payload is read proves that no payload was waited for or allocated.
struct OnlyTheHeader {
    header: [u8; 4],
    given: usize,
}

impl Read for OnlyTheHeader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.given >= 4 {
            panic!("the payload of an oversize frame was read");
        }
        let n = buf.len().min(4 - self.given);
        buf[..n].copy_from_slice(&self.header[self.given..self.given + n]);
        self.given += n;
        Ok(n)
    }
}

#[test]
fn tracer_a_length_over_the_cap_is_refused_before_any_payload_is_read() {
    let over = u32::try_from(MAX_PAYLOAD + 1).expect("fits");
    let mut reader = OnlyTheHeader { header: over.to_be_bytes(), given: 0 };
    assert!(matches!(read_frame(&mut reader), Err(WireError::TooLarge(len)) if len == MAX_PAYLOAD + 1));
    let mut reader = OnlyTheHeader { header: u32::MAX.to_be_bytes(), given: 0 };
    assert!(matches!(read_frame(&mut reader), Err(WireError::TooLarge(_))));
    // The cap itself is 16 MiB plus 64 bytes.
    assert_eq!(MAX_PAYLOAD, 16 * 1024 * 1024 + 64);
}

#[test]
fn tracer_the_codec_for_the_supervisor_reads_and_writes_the_same_bytes() {
    let frame = Frame::Cmd(CmdFrame { socket: 7, request: 1, json: r#"{"cmd":"key","args":{}}"#.into() });
    // The codec writes the prefix the blocking writer writes.
    let mut tokio_side = codec();
    let mut buffer = BytesMut::new();
    tokio_side.encode(&payload(&frame)[..], &mut buffer).expect("encodes");
    assert_eq!(buffer.to_vec(), on_the_wire(&frame));
    // And it reads what the blocking writer wrote, one frame at a time.
    let mut buffer = BytesMut::from(&on_the_wire(&frame)[..]);
    buffer.extend_from_slice(&on_the_wire(&Frame::Note(Note::Ready)));
    let first = tokio_side.decode(&mut buffer).expect("decodes").expect("the first frame");
    assert_eq!(Frame::from_payload(&first).expect("a frame"), frame);
    let second = tokio_side.decode(&mut buffer).expect("decodes").expect("the second frame");
    assert_eq!(Frame::from_payload(&second).expect("a frame"), Frame::Note(Note::Ready));
    // The codec refuses a length over the cap, as the blocking reader does.
    let mut over = BytesMut::from(&u32::try_from(MAX_PAYLOAD + 1).expect("fits").to_be_bytes()[..]);
    assert!(tokio_side.decode(&mut over).is_err());
}

#[test]
fn tracer_a_socket_pair_carries_a_command_to_a_worker_rig_and_an_answer_back() {
    // The rig itself speaks these frames: this is the smallest proof that its reader and writer use the pinned layout.
    let mut rig = worker_env::Rig::start();
    rig.init();
    let res = rig.call(7, "get_layout", serde_json::json!({}));
    assert!(matches!(res.body, ResBody::Ok(_)), "{res:?}");
    assert_eq!(res.socket, 7);
    rig.finish();
}

// ---- the rest of the layout ---------------------------------------------------------------------------------------------

const CTL_ID: u64 = 0x0102_0304_0506_0708;
const CTL_BYTES: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

fn key(n: u8) -> Key {
    Key::from_bytes(&[n; 32])
}

fn slot_key(kind: SlotKind, id: u8, n: u8) -> SlotKey {
    SlotKey { kind, id: [id; 16], key: key(n) }
}

/// The 49 bytes of a slot key.
fn slot_bytes(kind: u8, id: u8, n: u8) -> Vec<u8> {
    let mut out = vec![kind];
    out.extend_from_slice(&[id; 16]);
    out.extend_from_slice(&[n; 32]);
    out
}

/// Asserts that `frame` is exactly `expected` as a payload and decodes back to itself.
fn pinned(frame: Frame, expected: Vec<u8>) {
    assert_eq!(payload(&frame), expected, "{frame:?}");
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), frame);
}

#[test]
fn layout_detach_is_operation_3_then_the_socket() {
    pinned(Frame::Ctl(Ctl::Detach { socket: 0x0A0B_0C0D }), vec![0x04, 0x03, 0x0A, 0x0B, 0x0C, 0x0D]);
}

#[test]
fn layout_unlock_is_operation_4_then_control_id_key_kind_and_the_key() {
    let mut expected = vec![0x04, 0x04];
    expected.extend_from_slice(&CTL_BYTES);
    expected.push(1);
    expected.extend_from_slice(&[0xAB; 32]);
    assert_eq!(expected.len(), 43);
    pinned(Frame::Ctl(Ctl::Unlock { ctl: CTL_ID, kind: SlotKind::Password, key: key(0xAB) }), expected);
    // Every key kind has its number.
    for (kind, byte) in [(SlotKind::Password, 1u8), (SlotKind::Passphrase, 2), (SlotKind::Prf, 3), (SlotKind::Recovery, 4)] {
        let frame = Frame::Ctl(Ctl::Unlock { ctl: 1, kind, key: key(1) });
        assert_eq!(payload(&frame)[10], byte);
    }
}

#[test]
fn layout_lock_shutdown_and_reset_secrets() {
    let mut lock = vec![0x04, 0x05];
    lock.extend_from_slice(&CTL_BYTES);
    pinned(Frame::Ctl(Ctl::Lock { ctl: CTL_ID }), lock);
    pinned(Frame::Ctl(Ctl::Shutdown), vec![0x04, 0x06]);
    let mut reset = vec![0x04, 0x07];
    reset.extend_from_slice(&CTL_BYTES);
    pinned(Frame::Ctl(Ctl::ResetSecrets { ctl: CTL_ID }), reset);
}

#[test]
fn layout_provision_is_operation_12_with_a_count_and_that_many_slot_keys() {
    let mut none = vec![0x04, 0x0C];
    none.extend_from_slice(&CTL_BYTES);
    none.push(0);
    pinned(Frame::Ctl(Ctl::Provision { ctl: CTL_ID, slots: vec![] }), none);

    let mut one = vec![0x04, 0x0C];
    one.extend_from_slice(&CTL_BYTES);
    one.push(1);
    one.extend_from_slice(&slot_bytes(2, 7, 9));
    assert_eq!(one.len(), 11 + 49);
    pinned(Frame::Ctl(Ctl::Provision { ctl: CTL_ID, slots: vec![slot_key(SlotKind::Passphrase, 7, 9)] }), one);

    let mut four = vec![0x04, 0x0C];
    four.extend_from_slice(&CTL_BYTES);
    four.push(4);
    let kinds = [SlotKind::Password, SlotKind::Passphrase, SlotKind::Prf, SlotKind::Recovery];
    for (i, kind) in kinds.iter().enumerate() {
        let n = u8::try_from(i).expect("small") + 1;
        four.extend_from_slice(&slot_bytes(kind.byte(), n, n + 10));
    }
    assert_eq!(four.len(), 11 + 4 * 49);
    let slots = kinds.iter().enumerate().map(|(i, kind)| {
        let n = u8::try_from(i).expect("small") + 1;
        slot_key(*kind, n, n + 10)
    });
    pinned(Frame::Ctl(Ctl::Provision { ctl: CTL_ID, slots: slots.collect() }), four);
}

#[test]
fn layout_set_slot_is_operation_13_with_or_without_the_opening_key() {
    let mut without = vec![0x04, 0x0D];
    without.extend_from_slice(&CTL_BYTES);
    without.extend_from_slice(&slot_bytes(1, 3, 4));
    without.push(0);
    assert_eq!(without.len(), 60);
    pinned(
        Frame::Ctl(Ctl::SetSlot { ctl: CTL_ID, slot: slot_key(SlotKind::Password, 3, 4), opening: None }),
        without,
    );

    let mut with = vec![0x04, 0x0D];
    with.extend_from_slice(&CTL_BYTES);
    with.extend_from_slice(&slot_bytes(1, 3, 4));
    with.push(1);
    with.push(3);
    with.extend_from_slice(&[5; 32]);
    assert_eq!(with.len(), 93);
    pinned(
        Frame::Ctl(Ctl::SetSlot {
            ctl: CTL_ID,
            slot: slot_key(SlotKind::Password, 3, 4),
            opening: Some(OpeningKey { kind: SlotKind::Prf, key: key(5) }),
        }),
        with,
    );
}

#[test]
fn layout_remove_slot_is_operation_14_then_kind_and_the_slot_id() {
    let mut expected = vec![0x04, 0x0E];
    expected.extend_from_slice(&CTL_BYTES);
    expected.push(4);
    expected.extend_from_slice(&[0x66; 16]);
    assert_eq!(expected.len(), 27);
    pinned(Frame::Ctl(Ctl::RemoveSlot { ctl: CTL_ID, kind: SlotKind::Recovery, id: [0x66; 16] }), expected);
}

#[test]
fn layout_store_state_is_operation_2_with_one_byte_of_state() {
    pinned(Frame::Note(Note::StoreState(StoreState::Absent)), vec![0x05, 0x02, 0]);
    pinned(Frame::Note(Note::StoreState(StoreState::Locked)), vec![0x05, 0x02, 1]);
    pinned(Frame::Note(Note::StoreState(StoreState::Unlocked)), vec![0x05, 0x02, 2]);
}

#[test]
fn layout_pane_count_exiting_and_ctl_done() {
    pinned(Frame::Note(Note::PaneCount(0x0001_0203)), vec![0x05, 0x03, 0, 1, 2, 3]);
    pinned(Frame::Note(Note::Exiting(ExitReason::Clean)), vec![0x05, 0x04, 0]);
    pinned(Frame::Note(Note::Exiting(ExitReason::Failure)), vec![0x05, 0x04, 1]);
    for (outcome, byte) in [
        (CtlOutcome::Done, 0u8),
        (CtlOutcome::Locked, 1),
        (CtlOutcome::Refused, 2),
        (CtlOutcome::Failed, 3),
        (CtlOutcome::Absent, 4),
    ] {
        let mut expected = vec![0x05, 0x05];
        expected.extend_from_slice(&CTL_BYTES);
        expected.push(byte);
        pinned(Frame::Note(Note::CtlDone { ctl: CTL_ID, outcome }), expected);
    }
}

/// One frame of every shape with a fixed length.
fn fixed_frames() -> Vec<Frame> {
    vec![
        Frame::Ctl(Ctl::Init { version: 2, max_panes: 3, max_scrollback_lines: 100, max_image_mib: 64 }),
        Frame::Ctl(Ctl::Attach { socket: 9 }),
        Frame::Ctl(Ctl::Detach { socket: 9 }),
        Frame::Ctl(Ctl::Unlock { ctl: 5, kind: SlotKind::Prf, key: key(1) }),
        Frame::Ctl(Ctl::Lock { ctl: 5 }),
        Frame::Ctl(Ctl::Shutdown),
        Frame::Ctl(Ctl::ResetSecrets { ctl: 5 }),
        Frame::Ctl(Ctl::Provision { ctl: 5, slots: vec![slot_key(SlotKind::Password, 1, 1), slot_key(SlotKind::Recovery, 2, 2)] }),
        Frame::Ctl(Ctl::SetSlot { ctl: 5, slot: slot_key(SlotKind::Prf, 1, 1), opening: None }),
        Frame::Ctl(Ctl::SetSlot {
            ctl: 5,
            slot: slot_key(SlotKind::Prf, 1, 1),
            opening: Some(OpeningKey { kind: SlotKind::Password, key: key(2) }),
        }),
        Frame::Ctl(Ctl::RemoveSlot { ctl: 5, kind: SlotKind::Passphrase, id: [3; 16] }),
        Frame::Ctl(Ctl::FileServe { request: 5, what: FileWhat::Stat, token: TOKEN.into(), offset: 0, length: 0 }),
        Frame::Ctl(Ctl::FileServe { request: 5, what: FileWhat::Read, token: TOKEN.into(), offset: 7, length: 100 }),
        Frame::Ctl(Ctl::UploadClose { id: 5, commit: true }),
        Frame::Note(Note::Ready),
        Frame::Note(Note::StoreState(StoreState::Locked)),
        Frame::Note(Note::PaneCount(7)),
        Frame::Note(Note::Exiting(ExitReason::Failure)),
        Frame::Note(Note::CtlDone { ctl: 5, outcome: CtlOutcome::Refused }),
    ]
}

#[test]
fn truncation_at_every_length_of_every_control_and_note_is_an_error() {
    for frame in fixed_frames() {
        let full = payload(&frame);
        for cut in 0..full.len() {
            let result = Frame::from_payload(&full[..cut]);
            assert!(result.is_err(), "{frame:?} cut to {cut} of {} bytes decoded as {result:?}", full.len());
        }
        assert_eq!(Frame::from_payload(&full).expect("whole"), frame);
    }
}

#[test]
fn truncation_inside_the_head_of_a_command_an_answer_and_an_event_is_an_error() {
    let cmd = payload(&Frame::Cmd(CmdFrame { socket: 1, request: 2, json: "{}".into() }));
    let res = payload(&Frame::Res(Res { socket: 1, request: 2, body: ResBody::Ok("1".into()) }));
    let evt = payload(&Frame::Evt(Evt { target: 1, name: "title".into(), text: "{}".into() }));
    // The head is 13 bytes for a command, 14 for an answer; for an event it is 6 bytes and the name.
    for cut in 0..13 {
        assert!(Frame::from_payload(&cmd[..cut]).is_err(), "command cut to {cut}");
    }
    for cut in 0..14 {
        assert!(Frame::from_payload(&res[..cut]).is_err(), "answer cut to {cut}");
    }
    for cut in 0..(6 + 5) {
        assert!(Frame::from_payload(&evt[..cut]).is_err(), "event cut to {cut}");
    }
}

#[test]
fn a_trailing_byte_after_a_control_or_a_note_is_an_error() {
    for frame in fixed_frames() {
        let mut bytes = payload(&frame);
        bytes.push(0);
        assert!(matches!(Frame::from_payload(&bytes), Err(WireError::Trailing)), "{frame:?}");
    }
}

#[test]
fn the_four_numbers_once_reserved_are_the_file_controls_and_a_short_frame_of_each_is_cut_off() {
    // 8 to 11 carry the file controls now: an operation with nothing after it is a frame that is cut off, not a reserved
    // number, and no number is reserved any more.
    for op in 8u8..=11 {
        let error = Frame::from_payload(&[0x04, op]).expect_err("cut off");
        assert!(matches!(error, WireError::Truncated), "{op}: {error:?}");
    }
    let error = Frame::from_payload(&[0x04, 15]).expect_err("unknown");
    assert!(matches!(error, WireError::UnknownOp { kind: 4, op: 15 }), "{error:?}");
    assert!(matches!(WireError::ReservedOp(8).to_string().as_str(), "operation 8 is reserved"));
}

#[test]
fn an_unknown_kind_or_operation_is_an_error_never_a_panic() {
    for kind in [0u8, 6, 7, 0x7F, 0x80, 0xFF] {
        assert!(matches!(Frame::from_payload(&[kind, 0, 0, 0, 0]), Err(WireError::UnknownKind(k)) if k == kind));
    }
    for op in [0u8, 15, 16, 100, 255] {
        assert!(matches!(Frame::from_payload(&[0x04, op]), Err(WireError::UnknownOp { kind: 4, op: o }) if o == op), "ctl {op}");
    }
    for op in [0u8, 6, 7, 255] {
        assert!(matches!(Frame::from_payload(&[0x05, op]), Err(WireError::UnknownOp { kind: 5, op: o }) if o == op), "note {op}");
    }
    assert!(matches!(Frame::from_payload(&[]), Err(WireError::Empty)));
}

#[test]
fn values_out_of_range_are_errors() {
    // A key kind that is none of the four.
    let mut unlock = vec![0x04, 0x04];
    unlock.extend_from_slice(&CTL_BYTES);
    unlock.push(5);
    unlock.extend_from_slice(&[0; 32]);
    assert!(Frame::from_payload(&unlock).is_err());
    unlock[10] = 0;
    assert!(Frame::from_payload(&unlock).is_err());
    // Five slot keys in a Provision.
    let mut five = vec![0x04, 0x0C];
    five.extend_from_slice(&CTL_BYTES);
    five.push(5);
    for _ in 0..5 {
        five.extend_from_slice(&slot_bytes(1, 1, 1));
    }
    assert!(Frame::from_payload(&five).is_err());
    // An opening-key flag that is neither 0 nor 1.
    let mut set = vec![0x04, 0x0D];
    set.extend_from_slice(&CTL_BYTES);
    set.extend_from_slice(&slot_bytes(1, 1, 1));
    set.push(2);
    assert!(Frame::from_payload(&set).is_err());
    // A store state, an exit reason and an outcome that do not exist; a status of an answer too.
    assert!(Frame::from_payload(&[0x05, 0x02, 3]).is_err());
    assert!(Frame::from_payload(&[0x05, 0x04, 2]).is_err());
    let mut done = vec![0x05, 0x05];
    done.extend_from_slice(&CTL_BYTES);
    done.push(5);
    assert!(Frame::from_payload(&done).is_err());
    let mut res = vec![0x02, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 3];
    res.extend_from_slice(b"x");
    assert!(Frame::from_payload(&res).is_err());
}

#[test]
fn an_event_name_that_is_empty_or_not_lower_case_ascii_is_an_error() {
    let head = |name: &[u8]| {
        let mut out = vec![0x03, 0, 0, 0, 0, u8::try_from(name.len()).expect("short")];
        out.extend_from_slice(name);
        out.extend_from_slice(b"{}");
        out
    };
    for good in ["frame", "pane-exit", "login_prompt", "a"] {
        assert!(Frame::from_payload(&head(good.as_bytes())).is_ok(), "{good}");
    }
    for bad in [&b""[..], b"Frame", b"fr ame", b"frame1", b"fr/ame", b"fr.ame", b"fr\x00me", "fr\u{e9}me".as_bytes(), b"FRAME"] {
        assert!(matches!(Frame::from_payload(&head(bad)), Err(WireError::BadName)), "{bad:?}");
    }
    // The encoder refuses the same names and a name of 256 bytes; 255 is the longest.
    for bad in ["", "Frame", "a b", &"a".repeat(256)] {
        let frame = Frame::Evt(Evt { target: 0, name: bad.to_owned(), text: "{}".into() });
        assert!(frame.to_payload().is_err(), "{bad:?}");
    }
    let longest = Frame::Evt(Evt { target: 0, name: "a".repeat(255), text: "{}".into() });
    assert_eq!(Frame::from_payload(&payload(&longest)).expect("255 bytes"), longest);
}

#[test]
fn text_that_is_not_utf_8_is_an_error_in_every_field_that_is_text() {
    let mut cmd = vec![0x01, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
    cmd.extend_from_slice(&[0xFF, 0xFE]);
    assert!(matches!(Frame::from_payload(&cmd), Err(WireError::BadUtf8)));
    for status in [0u8, 1] {
        let mut res = vec![0x02, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, status];
        res.extend_from_slice(&[0xC0, 0x80]);
        assert!(matches!(Frame::from_payload(&res), Err(WireError::BadUtf8)), "status {status}");
    }
    // Status 2 is bytes: anything goes.
    let mut binary = vec![0x02, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 2];
    binary.extend_from_slice(&[0xFF, 0xFE, 0x00]);
    assert!(Frame::from_payload(&binary).is_ok());
    let mut evt = vec![0x03, 0, 0, 0, 0, 5];
    evt.extend_from_slice(b"frame");
    evt.extend_from_slice(&[0xFF]);
    assert!(matches!(Frame::from_payload(&evt), Err(WireError::BadUtf8)));
}

#[test]
fn socket_zero_is_refused_where_a_socket_is_required_and_means_all_in_an_event() {
    assert!(matches!(Frame::from_payload(&[0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]), Err(WireError::BadSocket)));
    assert!(matches!(Frame::from_payload(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0]), Err(WireError::BadSocket)));
    assert!(matches!(Frame::from_payload(&[0x04, 0x02, 0, 0, 0, 0]), Err(WireError::BadSocket)));
    assert!(matches!(Frame::from_payload(&[0x04, 0x03, 0, 0, 0, 0]), Err(WireError::BadSocket)));
    assert!(Frame::Cmd(CmdFrame { socket: 0, request: 1, json: "{}".into() }).to_payload().is_err());
    assert!(Frame::Ctl(Ctl::Attach { socket: 0 }).to_payload().is_err());
    assert!(Frame::Ctl(Ctl::Detach { socket: 0 }).to_payload().is_err());
    assert!(Frame::Res(Res { socket: 0, request: 1, body: ResBody::Ok("1".into()) }).to_payload().is_err());
    assert!(Frame::Evt(Evt { target: 0, name: "frame".into(), text: "{}".into() }).to_payload().is_ok());
}

#[test]
fn the_largest_frame_is_the_cap_and_one_byte_more_is_refused_by_the_encoder_and_the_reader() {
    // A payload of exactly the cap: 14 bytes of head and the rest of the body.
    let body = "x".repeat(MAX_PAYLOAD - 14);
    let frame = Frame::Res(Res { socket: 1, request: 1, body: ResBody::Ok(body) });
    let bytes = on_the_wire(&frame);
    assert_eq!(bytes.len(), 4 + MAX_PAYLOAD);
    assert_eq!(read_frame(&mut Cursor::new(bytes)).expect("read").expect("a frame"), frame);
    let too_big = Frame::Res(Res { socket: 1, request: 1, body: ResBody::Ok("x".repeat(MAX_PAYLOAD - 13)) });
    assert!(matches!(too_big.to_payload(), Err(WireError::TooLarge(_))));
    assert!(matches!(write_frame(&mut Vec::new(), &too_big), Err(WireError::TooLarge(_))));
}

#[test]
fn a_stream_that_ends_inside_a_frame_or_starts_one_with_no_payload_is_an_error() {
    // Two bytes of a four-byte length.
    assert!(matches!(read_frame(&mut Cursor::new(vec![0u8, 0])), Err(WireError::Truncated)));
    // A length of 10 and 3 bytes.
    assert!(matches!(read_frame(&mut Cursor::new(vec![0u8, 0, 0, 10, 1, 2, 3])), Err(WireError::Truncated)));
    // A length of zero.
    assert!(matches!(read_frame(&mut Cursor::new(vec![0u8, 0, 0, 0])), Err(WireError::Empty)));
    // A kind byte that is unknown, with a good length.
    assert!(matches!(read_frame(&mut Cursor::new(vec![0u8, 0, 0, 2, 9, 9])), Err(WireError::UnknownKind(9))));
    // Nothing at all is the clean end.
    assert!(read_frame(&mut Cursor::new(Vec::<u8>::new())).expect("clean").is_none());
}

#[test]
fn a_frame_that_holds_a_key_prints_none_of_it_in_debug_and_a_decode_error_neither() {
    // The key is text, so a printed byte of it can be found by reading.
    let canary: [u8; 32] = *b"canary-key-bytes-0123456789abcde";
    let key = Key::from_bytes(&canary);
    let frames = vec![
        Frame::Ctl(Ctl::Unlock { ctl: 1, kind: SlotKind::Password, key: key.clone() }),
        Frame::Ctl(Ctl::Provision { ctl: 2, slots: vec![SlotKey { kind: SlotKind::Prf, id: *b"canary-slot-id-0", key: key.clone() }] }),
        Frame::Ctl(Ctl::SetSlot {
            ctl: 3,
            slot: SlotKey { kind: SlotKind::Password, id: *b"canary-slot-id-1", key: key.clone() },
            opening: Some(OpeningKey { kind: SlotKind::Recovery, key: key.clone() }),
        }),
        Frame::Ctl(Ctl::RemoveSlot { ctl: 4, kind: SlotKind::Prf, id: *b"canary-slot-id-2" }),
    ];
    for frame in &frames {
        for text in [format!("{frame:?}"), format!("{frame:#?}")] {
            assert!(!text.contains("canary"), "{text}");
            for byte in canary {
                // Not one byte of the key as a decimal number in a list either (the ids and numbers are not the key).
                assert!(!text.contains(&format!("[{byte}")), "{text}");
            }
        }
        // A frame cut short fails with a sentence that holds none of it.
        let bytes = payload(frame);
        for cut in 0..bytes.len() {
            if let Err(error) = Frame::from_payload(&bytes[..cut]) {
                let sentence = format!("{error} {error:?}");
                assert!(!sentence.contains("canary"), "{sentence}");
            }
        }
    }
    assert_eq!(format!("{key:?}"), "Key(..)");
    assert!(!format!("{:?}", Ctl::Unlock { ctl: 1, kind: SlotKind::Password, key }).contains("canary"));
}

#[test]
fn a_key_is_wiped_when_dropped_and_the_buffers_of_a_frame_are_wiped_buffers() {
    fn wiped_on_drop<T: ZeroizeOnDrop>() {}
    wiped_on_drop::<Key>();
    // The encoded form of a key-bearing frame is a buffer that wipes itself.
    let encoded: Zeroizing<Vec<u8>> = Frame::Ctl(Ctl::Unlock { ctl: 1, kind: SlotKind::Password, key: key(7) })
        .to_payload()
        .expect("encodes");
    assert_eq!(encoded.len(), 43);
    // Keys compare by value.
    assert_eq!(key(3), key(3));
    assert_ne!(key(3), key(4));
}

/// A small deterministic generator (xorshift64*), so a failure can be run again.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn ten_thousand_garbage_payloads_never_panic_and_the_good_ones_survive_a_round_trip() {
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
    let mut decoded = 0usize;
    let real: Vec<Vec<u8>> = fixed_frames().iter().map(payload).collect();
    for case in 0..10_000 {
        let length = usize::try_from(noise.below(120)).expect("small");
        let mut bytes: Vec<u8> = (0..length).map(|_| u8::try_from(noise.below(256)).expect("a byte")).collect();
        // A third of the cases are a real frame with one byte changed, so the deep branches of the decoder run.
        if noise.below(3) == 0 {
            let which = usize::try_from(noise.below(real.len() as u64)).expect("small");
            bytes = real[which].clone();
            let at = usize::try_from(noise.below(bytes.len() as u64)).expect("small");
            match noise.below(3) {
                0 => bytes[at] = u8::try_from(noise.below(256)).expect("a byte"),
                1 => bytes.truncate(at),
                _ => bytes.push(0),
            }
        }
        // Most cases start like a real frame, so the deeper branches of the decoder run.
        if bytes.len() >= 2 && length >= 2 && noise.below(10) < 2 {
            bytes[0] = u8::try_from(1 + noise.below(5)).expect("a kind");
            if bytes[0] == 4 || bytes[0] == 5 {
                bytes[1] = u8::try_from(noise.below(18)).expect("an operation");
            }
        }
        if let Ok(frame) = Frame::from_payload(&bytes) {
            decoded += 1;
            // What decodes encodes again to the same frame.
            let again = Frame::from_payload(&payload(&frame)).expect("a decoded frame encodes and decodes");
            assert_eq!(again, frame, "case {case}");
        }
        // And through the reader, with a length prefix that is right.
        let mut stream = prefixed(&bytes);
        if bytes.is_empty() {
            stream = vec![0, 0, 0, 0];
        }
        let _ = read_frame(&mut Cursor::new(stream));
    }
    assert!(decoded > 100, "the generator reached the decoder: {decoded} frames decoded");
}

// ---- the file controls ----------------------------------------------------------------------------------------------------

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// The eight bytes of the request or upload id `0x0102030405060708`.
const ID_BYTES: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
const ID: u64 = 0x0102_0304_0506_0708;

#[test]
fn tracer_layout_file_serve_stat_is_operation_8_request_what_token_offset_length() {
    let frame = Frame::Ctl(Ctl::FileServe { request: ID, what: FileWhat::Stat, token: TOKEN.into(), offset: 0, length: 0 });
    let mut expected = vec![0x04, 0x08];
    expected.extend_from_slice(&ID_BYTES);
    expected.push(0);
    expected.extend_from_slice(TOKEN.as_bytes());
    expected.extend_from_slice(&[0; 8]);
    expected.extend_from_slice(&[0; 4]);
    assert_eq!(expected.len(), 55);
    pinned(frame, expected);
}

#[test]
fn tracer_layout_file_serve_read_carries_the_offset_and_the_length() {
    let frame =
        Frame::Ctl(Ctl::FileServe { request: ID, what: FileWhat::Read, token: TOKEN.into(), offset: 0x1112_1314_1516_1718, length: 0x0010_0000 });
    let mut expected = vec![0x04, 0x08];
    expected.extend_from_slice(&ID_BYTES);
    expected.push(1);
    expected.extend_from_slice(TOKEN.as_bytes());
    expected.extend_from_slice(&[0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18]);
    expected.extend_from_slice(&[0x00, 0x10, 0x00, 0x00]);
    pinned(frame, expected);
}

#[test]
fn tracer_layout_the_answer_to_a_file_control_is_a_res_on_the_file_socket() {
    assert_eq!(FILE_SOCKET, u32::MAX);
    let stat = Frame::Res(Res { socket: FILE_SOCKET, request: ID, body: ResBody::Ok(r#"{"mime":"image/png","size":8}"#.into()) });
    let mut expected = vec![0x02, 0xFF, 0xFF, 0xFF, 0xFF];
    expected.extend_from_slice(&ID_BYTES);
    expected.push(0);
    expected.extend_from_slice(br#"{"mime":"image/png","size":8}"#);
    pinned(stat, expected);

    let read = Frame::Res(Res { socket: FILE_SOCKET, request: ID, body: ResBody::Binary(vec![0x89, b'P', b'N', b'G']) });
    let mut expected = vec![0x02, 0xFF, 0xFF, 0xFF, 0xFF];
    expected.extend_from_slice(&ID_BYTES);
    expected.extend_from_slice(&[2, 0x89, b'P', b'N', b'G']);
    pinned(read, expected);

    let refused = Frame::Res(Res { socket: FILE_SOCKET, request: ID, body: ResBody::Err("\"no such file\"".into()) });
    let mut expected = vec![0x02, 0xFF, 0xFF, 0xFF, 0xFF];
    expected.extend_from_slice(&ID_BYTES);
    expected.push(1);
    expected.extend_from_slice(b"\"no such file\"");
    pinned(refused, expected);
}

#[test]
fn layout_upload_open_is_operation_9_id_size_folder_length_folder_and_the_name() {
    let frame = Frame::Ctl(Ctl::UploadOpen { id: ID, size: 0x0A0B, folder: "uploads".into(), name: "a.txt".into() });
    let mut expected = vec![0x04, 0x09];
    expected.extend_from_slice(&ID_BYTES);
    expected.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0x0A, 0x0B]);
    expected.extend_from_slice(&[0x00, 0x07]);
    expected.extend_from_slice(b"uploads");
    expected.extend_from_slice(b"a.txt");
    pinned(frame, expected);
}

#[test]
fn layout_upload_piece_is_operation_10_id_and_the_bytes() {
    let mut expected = vec![0x04, 0x0A];
    expected.extend_from_slice(&ID_BYTES);
    expected.extend_from_slice(&[1, 2, 3]);
    pinned(Frame::Ctl(Ctl::UploadPiece { id: ID, bytes: vec![1, 2, 3] }), expected);
    // A piece of exactly 1 MiB is the largest.
    let largest = Frame::Ctl(Ctl::UploadPiece { id: ID, bytes: vec![0xAB; FILE_PIECE] });
    let bytes = payload(&largest);
    assert_eq!(bytes.len(), 10 + FILE_PIECE);
    assert_eq!(Frame::from_payload(&bytes).expect("decodes"), largest);
}

#[test]
fn layout_upload_close_is_operation_11_id_and_the_commit_byte() {
    for (commit, byte) in [(false, 0u8), (true, 1u8)] {
        let mut expected = vec![0x04, 0x0B];
        expected.extend_from_slice(&ID_BYTES);
        expected.push(byte);
        pinned(Frame::Ctl(Ctl::UploadClose { id: ID, commit }), expected);
    }
}

/// The bytes of a stat or read of `FileServe` with the fields given raw, so that a test can build what no encoder would.
fn serve_bytes(request: u64, what: u8, token: &[u8], offset: u64, length: u32) -> Vec<u8> {
    let mut out = vec![0x04, 0x08];
    out.extend_from_slice(&request.to_be_bytes());
    out.push(what);
    out.extend_from_slice(token);
    out.extend_from_slice(&offset.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
    out
}

#[test]
fn a_file_serve_with_a_bad_token_what_offset_length_or_id_is_an_error_and_never_a_panic() {
    let good = serve_bytes(1, 1, TOKEN.as_bytes(), 0, 10);
    assert!(Frame::from_payload(&good).is_ok());
    let bad_tokens: [&[u8]; 8] = [
        b"0123456789abcdef0123456789abcde",   // 31
        b"0123456789abcdef0123456789abcdef0", // 33 (trailing)
        b"0123456789ABCDEF0123456789abcdef",  // upper case
        b"0123456789abcdef0123456789abcdeg",  // not hex
        b"0123456789abcdef0123456789abcde.",  // a dot
        b"0123456789abcdef0123456789abcde/",  // a slash
        b"0123456789abcdef0123456789abcde\0", // a NUL
        b"\xff123456789abcdef0123456789abcde", // not UTF-8
    ];
    for token in bad_tokens {
        assert!(Frame::from_payload(&serve_bytes(1, 1, token, 0, 10)).is_err(), "{token:?}");
    }
    assert!(Frame::from_payload(&serve_bytes(1, 2, TOKEN.as_bytes(), 0, 10)).is_err(), "what 2");
    assert!(Frame::from_payload(&serve_bytes(1, 255, TOKEN.as_bytes(), 0, 10)).is_err(), "what 255");
    assert!(Frame::from_payload(&serve_bytes(0, 1, TOKEN.as_bytes(), 0, 10)).is_err(), "request 0");
    assert!(Frame::from_payload(&serve_bytes(1, 0, TOKEN.as_bytes(), 1, 0)).is_err(), "a stat with an offset");
    assert!(Frame::from_payload(&serve_bytes(1, 0, TOKEN.as_bytes(), 0, 1)).is_err(), "a stat with a length");
    assert!(Frame::from_payload(&serve_bytes(1, 1, TOKEN.as_bytes(), 0, 0)).is_err(), "a read of nothing");
    let piece = u32::try_from(FILE_PIECE).expect("small");
    assert!(Frame::from_payload(&serve_bytes(1, 1, TOKEN.as_bytes(), 0, piece)).is_ok(), "a read of 1 MiB");
    assert!(Frame::from_payload(&serve_bytes(1, 1, TOKEN.as_bytes(), 0, piece + 1)).is_err(), "over 1 MiB");
    let chunk = u32::try_from(holzwave_host::ranges::CHUNK).expect("small");
    assert!(Frame::from_payload(&serve_bytes(1, 1, TOKEN.as_bytes(), 0, chunk + 1)).is_err(), "over the chunk limit");
    assert!(Frame::from_payload(&serve_bytes(1, 1, TOKEN.as_bytes(), 0, u32::MAX)).is_err(), "u32::MAX");
    // The encoder refuses the same.
    for (what, offset, length, token) in [
        (FileWhat::Read, 0, 0, TOKEN),
        (FileWhat::Read, 0, piece + 1, TOKEN),
        (FileWhat::Stat, 1, 0, TOKEN),
        (FileWhat::Stat, 0, 1, TOKEN),
        (FileWhat::Read, 0, 1, "0123456789ABCDEF0123456789abcdef"),
        (FileWhat::Read, 0, 1, "short"),
    ] {
        let frame = Frame::Ctl(Ctl::FileServe { request: 1, what, token: token.into(), offset, length });
        assert!(frame.to_payload().is_err(), "{frame:?}");
    }
    assert!(Frame::Ctl(Ctl::FileServe { request: 0, what: FileWhat::Stat, token: TOKEN.into(), offset: 0, length: 0 })
        .to_payload()
        .is_err());
}

#[test]
fn the_upload_controls_refuse_an_id_of_0_an_empty_or_too_long_part_and_a_bad_commit_byte() {
    let open = |id: u64, folder_len: u16, folder: &[u8], name: &[u8]| {
        let mut out = vec![0x04, 0x09];
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&5u64.to_be_bytes());
        out.extend_from_slice(&folder_len.to_be_bytes());
        out.extend_from_slice(folder);
        out.extend_from_slice(name);
        out
    };
    assert!(Frame::from_payload(&open(1, 7, b"uploads", b"a.txt")).is_ok());
    assert!(Frame::from_payload(&open(0, 7, b"uploads", b"a.txt")).is_err(), "id 0");
    assert!(Frame::from_payload(&open(1, 0, b"", b"a.txt")).is_err(), "no folder");
    assert!(Frame::from_payload(&open(1, 1025, &[b'a'; 1025], b"a.txt")).is_err(), "a folder over 1024");
    assert!(Frame::from_payload(&open(1, 1024, &[b'a'; 1024], b"a.txt")).is_ok(), "a folder of 1024");
    assert!(Frame::from_payload(&open(1, 7, b"uploads", b"")).is_err(), "no name");
    assert!(Frame::from_payload(&open(1, 7, b"uploads", &[b'a'; 256])).is_err(), "a name over 255");
    assert!(Frame::from_payload(&open(1, 7, b"uploads", &[b'a'; 255])).is_ok(), "a name of 255");
    assert!(Frame::from_payload(&open(1, 9, b"uploads", b"a")).is_err(), "a folder longer than the frame");
    assert!(Frame::from_payload(&open(1, 7, b"upl\xffads", b"a.txt")).is_err(), "a folder that is not UTF-8");
    assert!(Frame::from_payload(&open(1, 7, b"uploads", b"a\xff")).is_err(), "a name that is not UTF-8");
    for cut in 0..19 {
        assert!(Frame::from_payload(&open(1, 7, b"uploads", b"a.txt")[..cut]).is_err(), "cut to {cut}");
    }

    let mut piece = vec![0x04, 0x0A];
    piece.extend_from_slice(&0u64.to_be_bytes());
    piece.push(1);
    assert!(Frame::from_payload(&piece).is_err(), "piece id 0");
    let mut empty = vec![0x04, 0x0A];
    empty.extend_from_slice(&ID_BYTES);
    assert!(Frame::from_payload(&empty).is_err(), "an empty piece");
    let mut over = empty.clone();
    over.extend(std::iter::repeat_n(0u8, FILE_PIECE + 1));
    assert!(Frame::from_payload(&over).is_err(), "a piece over 1 MiB");
    assert!(Frame::Ctl(Ctl::UploadPiece { id: 1, bytes: Vec::new() }).to_payload().is_err());
    assert!(Frame::Ctl(Ctl::UploadPiece { id: 1, bytes: vec![0; FILE_PIECE + 1] }).to_payload().is_err());
    assert!(Frame::Ctl(Ctl::UploadPiece { id: 0, bytes: vec![0] }).to_payload().is_err());

    let mut close = vec![0x04, 0x0B];
    close.extend_from_slice(&ID_BYTES);
    close.push(2);
    assert!(Frame::from_payload(&close).is_err(), "a commit byte of 2");
    let mut close0 = vec![0x04, 0x0B];
    close0.extend_from_slice(&0u64.to_be_bytes());
    close0.push(1);
    assert!(Frame::from_payload(&close0).is_err(), "close id 0");
    assert!(Frame::Ctl(Ctl::UploadOpen { id: 0, size: 1, folder: "a".into(), name: "b".into() }).to_payload().is_err());
    assert!(Frame::Ctl(Ctl::UploadOpen { id: 1, size: 1, folder: String::new(), name: "b".into() }).to_payload().is_err());
    assert!(Frame::Ctl(Ctl::UploadOpen { id: 1, size: 1, folder: "a".into(), name: String::new() }).to_payload().is_err());
    assert!(Frame::Ctl(Ctl::UploadClose { id: 0, commit: false }).to_payload().is_err());
}

#[test]
fn the_debug_text_of_a_file_control_holds_no_token_no_folder_no_name_and_no_byte() {
    let serve = Frame::Ctl(Ctl::FileServe { request: 3, what: FileWhat::Read, token: TOKEN.into(), offset: 1, length: 2 });
    let open = Frame::Ctl(Ctl::UploadOpen { id: 4, size: 5, folder: "SECRET-FOLDER".into(), name: "SECRET-NAME".into() });
    let piece = Frame::Ctl(Ctl::UploadPiece { id: 4, bytes: b"SECRET-BYTES".to_vec() });
    for frame in [serve, open, piece] {
        for text in [format!("{frame:?}"), format!("{frame:#?}")] {
            assert!(!text.contains(TOKEN) && !text.contains("SECRET"), "{text}");
        }
    }
}

#[test]
fn a_garbage_stream_of_file_controls_never_panics() {
    let mut noise = Noise(0x1234_5678_9ABC_DEF1);
    let templates = [
        serve_bytes(1, 1, TOKEN.as_bytes(), 3, 100),
        serve_bytes(1, 0, TOKEN.as_bytes(), 0, 0),
        payload(&Frame::Ctl(Ctl::UploadOpen { id: 1, size: 9, folder: "uploads".into(), name: "a.txt".into() })),
        payload(&Frame::Ctl(Ctl::UploadPiece { id: 1, bytes: vec![1, 2, 3, 4] })),
        payload(&Frame::Ctl(Ctl::UploadClose { id: 1, commit: true })),
    ];
    let mut decoded = 0;
    for _ in 0..5_000 {
        let mut bytes = templates[usize::try_from(noise.below(templates.len() as u64)).expect("small")].clone();
        let at = usize::try_from(noise.below(bytes.len() as u64)).expect("small");
        match noise.below(3) {
            0 => bytes[at] = u8::try_from(noise.below(256)).expect("a byte"),
            1 => bytes.truncate(at),
            _ => bytes.push(0),
        }
        if let Ok(frame) = Frame::from_payload(&bytes) {
            decoded += 1;
            assert_eq!(Frame::from_payload(&payload(&frame)).expect("again"), frame);
        }
    }
    assert!(decoded > 100, "{decoded}");
}
