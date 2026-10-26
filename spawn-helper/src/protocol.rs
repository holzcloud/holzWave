//! The messages between the supervisor and the helper.
//!
//! One JSON object per datagram on a `SOCK_SEQPACKET` socket, at most [`MAX_MESSAGE`] bytes. Every request carries an
//! `id` that its answer echoes. A refusal is `{"id":N,"ok":false,"error":"<code>"}` where the code is one of a fixed set
//! of words ([`Refusal::code`]): no refusal ever contains a byte of the request. The helper also sends events that carry
//! no id: `{"event":"exited","pid":P,"uid":U,"status":S,"signal":G}`.
//!
//! Requests:
//!
//! | `op`          | members                          | answer members                         |
//! |---------------|----------------------------------|----------------------------------------|
//! | `spawn`       | `uid`, `mode`                    | `pid`, and the worker's socket end as `SCM_RIGHTS` |
//! | `kill_uid`    | `uid`, `signal` (`term`, `kill`) | `signalled`                            |
//! | `remove_home` | `uid`                            | none                                   |
//! | `ping`        |                                  | `workers`                              |

use serde::{Deserialize, Serialize};

/// The largest message in either direction. A bigger request is refused without being read further.
pub const MAX_MESSAGE: usize = 4096;

/// What a worker is asked to do. The helper passes the word to the worker program as `worker --mode <word>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Serve a person's terminal sessions.
    Serve,
    /// Remove the person's secret store, then exit.
    WipeSecrets,
    /// Remove everything inside the person's home, then exit.
    WipeHome,
}

impl Mode {
    /// The word on the wire and on the worker's command line.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Serve => "serve",
            Mode::WipeSecrets => "wipe_secrets",
            Mode::WipeHome => "wipe_home",
        }
    }

    /// The mode a word names, if it is one of the three.
    pub fn parse(word: &str) -> Option<Mode> {
        match word {
            "serve" => Some(Mode::Serve),
            "wipe_secrets" => Some(Mode::WipeSecrets),
            "wipe_home" => Some(Mode::WipeHome),
            _ => None,
        }
    }
}

/// The signal of a `kill_uid` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// `SIGTERM`.
    Term,
    /// `SIGKILL`.
    Kill,
}

/// A request that parsed. The user id is as large as the wire allows; the helper checks it against its range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Start a worker for a person.
    Spawn { uid: u64, mode: Mode },
    /// Signal every process of a person.
    KillUid { uid: u64, signal: Signal },
    /// Remove the home of a person.
    RemoveHome { uid: u64 },
    /// Ask whether the helper is alive and how many workers it holds.
    Ping,
}

/// Why a request was not carried out. Each variant has a fixed code word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The message is longer than [`MAX_MESSAGE`].
    TooLarge,
    /// The message is not a request: not UTF-8, not JSON, an unknown or duplicate member, a wrong type or a missing member.
    BadRequest,
    /// The `op` is none of the four.
    UnknownOp,
    /// The user id is outside the range the helper was started with.
    UidRefused,
    /// The mode is none of the three.
    ModeRefused,
    /// A worker of that user is alive.
    BusyUid,
    /// The helper holds as many workers as it may.
    WorkerLimit,
    /// The person has no home, and the operation needs one.
    NoHome,
    /// The entry at the home path is not a private directory of that user, or could not be made one.
    HomeRefused,
    /// The worker could not be started.
    SpawnFailed,
    /// The wipe worker ended with a failure.
    WipeFailed,
    /// The wipe worker did not finish in time and was killed.
    WipeTimeout,
    /// The emptied home directory could not be removed.
    RemoveFailed,
    /// The processes of the person could not be listed or signalled.
    KillFailed,
}

impl Refusal {
    /// The fixed word sent as `error`.
    pub fn code(self) -> &'static str {
        match self {
            Refusal::TooLarge => "too_large",
            Refusal::BadRequest => "bad_request",
            Refusal::UnknownOp => "unknown_op",
            Refusal::UidRefused => "uid_refused",
            Refusal::ModeRefused => "mode_refused",
            Refusal::BusyUid => "busy_uid",
            Refusal::WorkerLimit => "worker_limit",
            Refusal::NoHome => "no_home",
            Refusal::HomeRefused => "home_refused",
            Refusal::SpawnFailed => "spawn_failed",
            Refusal::WipeFailed => "wipe_failed",
            Refusal::WipeTimeout => "wipe_timeout",
            Refusal::RemoveFailed => "remove_failed",
            Refusal::KillFailed => "kill_failed",
        }
    }
}

/// Every member a request may have. `deny_unknown_fields` makes any other member a parse error, and the derived
/// deserializer reports a repeated member as an error too.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    id: u64,
    op: String,
    uid: Option<u64>,
    mode: Option<String>,
    signal: Option<String>,
}

/// The id of a message that did not parse as a request, if it has a numeric one, so that the refusal can be matched to
/// the request. 0 when there is none.
fn salvage_id(bytes: &[u8]) -> u64 {
    serde_json::from_slice::<serde_json::Value>(bytes).ok().and_then(|v| v.get("id").and_then(serde_json::Value::as_u64)).unwrap_or(0)
}

/// Parses one datagram. On a refusal the id to answer with comes along (0 when the message has none).
pub fn parse(bytes: &[u8]) -> Result<(u64, Request), (u64, Refusal)> {
    if bytes.len() > MAX_MESSAGE {
        return Err((0, Refusal::TooLarge));
    }
    let raw: Raw = match serde_json::from_slice(bytes) {
        Ok(raw) => raw,
        Err(_) => return Err((salvage_id(bytes), Refusal::BadRequest)),
    };
    let id = raw.id;
    let bad = Err((id, Refusal::BadRequest));
    match raw.op.as_str() {
        "ping" => {
            if raw.uid.is_some() || raw.mode.is_some() || raw.signal.is_some() {
                return bad;
            }
            Ok((id, Request::Ping))
        }
        "spawn" => {
            let (Some(uid), Some(mode)) = (raw.uid, raw.mode.as_deref()) else { return bad };
            if raw.signal.is_some() {
                return bad;
            }
            match Mode::parse(mode) {
                Some(mode) => Ok((id, Request::Spawn { uid, mode })),
                None => Err((id, Refusal::ModeRefused)),
            }
        }
        "kill_uid" => {
            let (Some(uid), Some(signal)) = (raw.uid, raw.signal.as_deref()) else { return bad };
            if raw.mode.is_some() {
                return bad;
            }
            let signal = match signal {
                "term" => Signal::Term,
                "kill" => Signal::Kill,
                _ => return bad,
            };
            Ok((id, Request::KillUid { uid, signal }))
        }
        "remove_home" => {
            let Some(uid) = raw.uid else { return bad };
            if raw.mode.is_some() || raw.signal.is_some() {
                return bad;
            }
            Ok((id, Request::RemoveHome { uid }))
        }
        _ => Err((id, Refusal::UnknownOp)),
    }
}

/// An answer to a request.
#[derive(Debug, Serialize)]
pub struct Answer {
    /// The id of the request.
    pub id: u64,
    /// Whether the request was carried out.
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signalled: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workers: Option<u32>,
}

impl Answer {
    fn base(id: u64, ok: bool) -> Answer {
        Answer { id, ok, error: None, pid: None, signalled: None, workers: None }
    }

    /// A success with nothing to report.
    pub fn done(id: u64) -> Answer {
        Answer::base(id, true)
    }

    /// A refusal, with the fixed code word.
    pub fn refused(id: u64, refusal: Refusal) -> Answer {
        Answer { error: Some(refusal.code()), ..Answer::base(id, false) }
    }

    /// A worker was started.
    pub fn spawned(id: u64, pid: i32) -> Answer {
        Answer { pid: Some(pid), ..Answer::base(id, true) }
    }

    /// Processes were signalled.
    pub fn signalled(id: u64, count: u32) -> Answer {
        Answer { signalled: Some(count), ..Answer::base(id, true) }
    }

    /// The reply to `ping`.
    pub fn pong(id: u64, workers: u32) -> Answer {
        Answer { workers: Some(workers), ..Answer::base(id, true) }
    }

    /// The bytes of the datagram.
    pub fn to_bytes(&self) -> Vec<u8> {
        // A struct of numbers and fixed words always serializes.
        serde_json::to_vec(self).unwrap_or_default()
    }
}

/// The event sent when a worker has ended.
#[derive(Debug, Serialize)]
pub struct ExitedEvent {
    event: &'static str,
    /// The process id of the worker.
    pub pid: i32,
    /// The user id of the person.
    pub uid: u32,
    /// The exit code, or 128 plus the signal number when a signal ended the worker.
    pub status: i32,
    /// The signal that ended the worker, if one did.
    pub signal: Option<i32>,
}

impl ExitedEvent {
    /// An `exited` event.
    pub fn new(pid: i32, uid: u32, status: i32, signal: Option<i32>) -> ExitedEvent {
        ExitedEvent { event: "exited", pid, uid, status, signal }
    }

    /// The bytes of the datagram.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_four_operations() {
        assert_eq!(
            parse(br#"{"id":1,"op":"spawn","uid":20001,"mode":"serve"}"#),
            Ok((1, Request::Spawn { uid: 20001, mode: Mode::Serve }))
        );
        assert_eq!(
            parse(br#"{"id":2,"op":"kill_uid","uid":20001,"signal":"kill"}"#),
            Ok((2, Request::KillUid { uid: 20001, signal: Signal::Kill }))
        );
        assert_eq!(parse(br#"{"id":3,"op":"remove_home","uid":20001}"#), Ok((3, Request::RemoveHome { uid: 20001 })));
        assert_eq!(parse(br#"{"id":4,"op":"ping"}"#), Ok((4, Request::Ping)));
    }

    #[test]
    fn refuses_unknown_things() {
        assert_eq!(parse(br#"{"id":5,"op":"exec"}"#), Err((5, Refusal::UnknownOp)));
        assert_eq!(parse(br#"{"id":6,"op":"ping","extra":1}"#), Err((6, Refusal::BadRequest)));
        assert_eq!(parse(br#"{"op":"ping"}"#), Err((0, Refusal::BadRequest)));
        assert_eq!(parse(br#"{"id":7,"op":"spawn","uid":20001,"mode":"shell"}"#), Err((7, Refusal::ModeRefused)));
    }

    #[test]
    fn refuses_more_than_four_kilobytes() {
        let mut big = br#"{"id":8,"op":"ping","pad":""#.to_vec();
        big.resize(MAX_MESSAGE + 1, b'a');
        assert_eq!(parse(&big), Err((0, Refusal::TooLarge)));
        assert_eq!(parse(&big[..MAX_MESSAGE]).unwrap_err().1, Refusal::BadRequest, "exactly 4096 is read, and is not a request here");
    }

    #[test]
    fn a_refusal_names_no_byte_of_the_request() {
        let a = Answer::refused(9, Refusal::UidRefused).to_bytes();
        assert_eq!(a, br#"{"id":9,"ok":false,"error":"uid_refused"}"#);
    }

    #[test]
    fn the_answers_have_the_documented_shape() {
        assert_eq!(Answer::done(1).to_bytes(), br#"{"id":1,"ok":true}"#);
        assert_eq!(Answer::spawned(2, 77).to_bytes(), br#"{"id":2,"ok":true,"pid":77}"#);
        assert_eq!(Answer::signalled(3, 4).to_bytes(), br#"{"id":3,"ok":true,"signalled":4}"#);
        assert_eq!(Answer::pong(4, 2).to_bytes(), br#"{"id":4,"ok":true,"workers":2}"#);
        assert_eq!(
            ExitedEvent::new(5, 20001, 137, Some(9)).to_bytes(),
            br#"{"event":"exited","pid":5,"uid":20001,"status":137,"signal":9}"#
        );
    }
}
