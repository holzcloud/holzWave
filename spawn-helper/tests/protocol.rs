//! The request parser and the answers, through the public interface.

use holzwave_spawn_helper::protocol::{Answer, ExitedEvent, MAX_MESSAGE, Mode, Refusal, Request, Signal, parse};

fn refused(bytes: &[u8]) -> Refusal {
    match parse(bytes) {
        Err((_, refusal)) => refusal,
        Ok(parsed) => panic!("{:?} was accepted as {parsed:?}", String::from_utf8_lossy(bytes)),
    }
}

#[test]
fn a_valid_request_of_each_operation() {
    assert_eq!(
        parse(br#"{"id":1,"op":"spawn","uid":20001,"mode":"wipe_secrets"}"#),
        Ok((1, Request::Spawn { uid: 20001, mode: Mode::WipeSecrets }))
    );
    assert_eq!(
        parse(br#"{"uid":20001,"signal":"term","op":"kill_uid","id":18446744073709551615}"#),
        Ok((u64::MAX, Request::KillUid { uid: 20001, signal: Signal::Term }))
    );
    assert_eq!(parse(br#" {"id":3, "op":"remove_home", "uid":4294967296} "#), Ok((3, Request::RemoveHome { uid: 4294967296 })));
    assert_eq!(parse(br#"{"id":0,"op":"ping"}"#), Ok((0, Request::Ping)));
}

#[test]
fn size_is_bounded_at_four_kilobytes() {
    assert_eq!(MAX_MESSAGE, 4096);
    let mut message = br#"{"id":1,"op":"ping","x":""#.to_vec();
    message.resize(MAX_MESSAGE + 1, b' ');
    assert_eq!(refused(&message), Refusal::TooLarge);
    assert_eq!(parse(&message), Err((0, Refusal::TooLarge)), "an oversize message is not read for an id");
    message.truncate(MAX_MESSAGE);
    assert_eq!(refused(&message), Refusal::BadRequest, "exactly the limit is looked at, and this one is cut off");
    let huge = vec![b'{'; 1 << 20];
    assert_eq!(refused(&huge), Refusal::TooLarge);
}

#[test]
fn truncated_non_utf8_and_non_json_input() {
    for bytes in [
        &b""[..],
        b"{",
        br#"{"id":1,"op":"pi"#,
        br#"{"id":1,"op":"ping""#,
        &[0xff, 0xfe, 0xfd, 0x00],
        b"\xef\xbb\xbf{\"id\":1,\"op\":\"ping\"}",
        b"not json",
        b"null",
        b"[]",
        b"1",
        br#""ping""#,
        br#"{"id":1,"op":"ping"} {"id":2,"op":"ping"}"#,
        br#"{"id":1,"op":"ping"}x"#,
        b"{\"id\":1,\"op\":\"ping\xff\"}",
    ] {
        assert_eq!(refused(bytes), Refusal::BadRequest, "{:?}", String::from_utf8_lossy(bytes));
    }
}

#[test]
fn unknown_operations_fields_types_and_duplicates() {
    assert_eq!(refused(br#"{"id":1,"op":"exec"}"#), Refusal::UnknownOp);
    assert_eq!(refused(br#"{"id":1,"op":""}"#), Refusal::UnknownOp);
    assert_eq!(refused(br#"{"id":1,"op":"SPAWN","uid":20001,"mode":"serve"}"#), Refusal::UnknownOp);
    for bytes in [
        &br#"{"id":1,"op":"ping","extra":true}"#[..],
        br#"{"id":1,"op":"spawn","uid":20001,"mode":"serve","home":"/etc"}"#,
        br#"{"id":1,"op":"spawn","uid":20001,"mode":"serve","worker_path":"/bin/sh"}"#,
        br#"{"id":1,"op":"spawn","uid":20001,"mode":"serve","range":[0,5]}"#,
        br#"{"id":"1","op":"ping"}"#,
        br#"{"id":-1,"op":"ping"}"#,
        br#"{"id":1.5,"op":"ping"}"#,
        br#"{"op":"ping"}"#,
        br#"{"id":1}"#,
        br#"{"id":1,"op":7}"#,
        br#"{"id":1,"op":null}"#,
        br#"{"id":1,"op":"spawn","uid":"20001","mode":"serve"}"#,
        br#"{"id":1,"op":"spawn","uid":-5,"mode":"serve"}"#,
        br#"{"id":1,"op":"spawn","uid":20001.0,"mode":"serve"}"#,
        br#"{"id":1,"op":"spawn","uid":20001,"mode":7}"#,
        br#"{"id":1,"op":"spawn","uid":null,"mode":"serve"}"#,
        br#"{"id":1,"op":"spawn","uid":20001}"#,
        br#"{"id":1,"op":"spawn","mode":"serve"}"#,
        br#"{"id":1,"op":"spawn","uid":20001,"mode":"serve","signal":"kill"}"#,
        br#"{"id":1,"op":"kill_uid","uid":20001}"#,
        br#"{"id":1,"op":"kill_uid","uid":20001,"signal":"hup"}"#,
        br#"{"id":1,"op":"kill_uid","uid":20001,"signal":"kill","mode":"serve"}"#,
        br#"{"id":1,"op":"remove_home"}"#,
        br#"{"id":1,"op":"remove_home","uid":20001,"mode":"wipe_home"}"#,
        br#"{"id":1,"op":"ping","uid":20001}"#,
        br#"{"id":1,"id":2,"op":"ping"}"#,
        br#"{"id":1,"op":"ping","op":"ping"}"#,
        br#"{"id":1,"op":"spawn","uid":20001,"uid":20002,"mode":"serve"}"#,
    ] {
        assert_eq!(refused(bytes), Refusal::BadRequest, "{:?}", String::from_utf8_lossy(bytes));
    }
    assert_eq!(refused(br#"{"id":1,"op":"spawn","uid":20001,"mode":"shell"}"#), Refusal::ModeRefused);
    assert_eq!(refused(br#"{"id":1,"op":"spawn","uid":20001,"mode":"Serve"}"#), Refusal::ModeRefused);
}

#[test]
fn the_id_of_a_bad_request_comes_back_when_it_has_one() {
    assert_eq!(parse(br#"{"id":7,"op":"ping","extra":1}"#), Err((7, Refusal::BadRequest)));
    assert_eq!(parse(br#"{"id":7,"op":"exec"}"#), Err((7, Refusal::UnknownOp)));
    assert_eq!(parse(br#"{"op":"ping"}"#), Err((0, Refusal::BadRequest)));
    assert_eq!(parse(b"garbage"), Err((0, Refusal::BadRequest)));
}

#[test]
fn every_refusal_is_a_fixed_word_that_holds_no_byte_of_the_request() {
    let all = [
        Refusal::TooLarge,
        Refusal::BadRequest,
        Refusal::UnknownOp,
        Refusal::UidRefused,
        Refusal::ModeRefused,
        Refusal::BusyUid,
        Refusal::WorkerLimit,
        Refusal::NoHome,
        Refusal::HomeRefused,
        Refusal::SpawnFailed,
        Refusal::WipeFailed,
        Refusal::WipeTimeout,
        Refusal::RemoveFailed,
        Refusal::KillFailed,
    ];
    let mut seen = std::collections::BTreeSet::new();
    for refusal in all {
        let code = refusal.code();
        assert!(code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{code}");
        assert!(seen.insert(code), "{code} is used twice");
        let bytes = Answer::refused(42, refusal).to_bytes();
        assert_eq!(bytes, format!(r#"{{"id":42,"ok":false,"error":"{code}"}}"#).into_bytes());
    }
    // The request text never reaches the answer: the answer is built from the id and the code alone.
    let hostile = br#"{"id":9,"op":"<script>alert(1)</script>","uid":20001}"#;
    let (id, refusal) = parse(hostile).unwrap_err();
    let answer = String::from_utf8(Answer::refused(id, refusal).to_bytes()).unwrap();
    assert_eq!(answer, r#"{"id":9,"ok":false,"error":"unknown_op"}"#);
}

#[test]
fn answers_and_events_stay_small_and_have_the_documented_members() {
    assert_eq!(Answer::spawned(1, 123).to_bytes(), br#"{"id":1,"ok":true,"pid":123}"#);
    assert_eq!(Answer::signalled(1, 3).to_bytes(), br#"{"id":1,"ok":true,"signalled":3}"#);
    assert_eq!(Answer::pong(1, 0).to_bytes(), br#"{"id":1,"ok":true,"workers":0}"#);
    assert_eq!(Answer::done(1).to_bytes(), br#"{"id":1,"ok":true}"#);
    let event = ExitedEvent::new(77, 20001, 0, None).to_bytes();
    assert_eq!(event, br#"{"event":"exited","pid":77,"uid":20001,"status":0,"signal":null}"#);
    assert!(event.len() < 256);
}
