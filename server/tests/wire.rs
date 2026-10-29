//! The frames between the supervisor and a worker: the layout pinned byte for byte, round trips, truncation and garbage.
//!
//! The layout is written in the doc comment of `server/src/wire.rs`; every pin below is the literal bytes of one frame.

#[path = "support/scratch.rs"]
mod scratch;
#[path = "support/worker_env.rs"]
mod worker_env;

use std::io::{self, Cursor, Read};

use holzwave_server::wire::{
    CmdFrame, Ctl, Evt, Frame, MAX_PAYLOAD, Note, Res, ResBody, WireError, codec, read_frame, write_frame,
};
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
fn tracer_init_is_kind_4_operation_1_then_version_and_the_two_caps() {
    let frame = Frame::Ctl(Ctl::Init { version: 1, max_panes: 3, max_scrollback_lines: 0x0001_0000 });
    let expected = vec![0x04, 0x01, 0, 1, 0, 0, 0, 3, 0, 1, 0, 0];
    assert_eq!(payload(&frame), expected);
    assert_eq!(Frame::from_payload(&expected).expect("decodes"), frame);
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
        Frame::Ctl(Ctl::Init { version: 1, max_panes: 0, max_scrollback_lines: 0 }),
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
