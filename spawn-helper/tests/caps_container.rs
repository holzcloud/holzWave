//! What a real kernel says about the helper and the workers it starts.
//!
//! These tests need the container that `scripts/web-container-test.sh caps` builds: a Linux user 10001 with every
//! capability dropped except `CAP_SETUID`, `CAP_SETGID`, `CAP_CHOWN` and `CAP_KILL` added, the helper file carrying the
//! file capabilities `cap_setuid,cap_setgid,cap_chown,cap_kill=p` (and a second copy that does not), and a stand-in
//! worker script (`tests/support/fake-worker.sh`) that reports what it was left with. The test process plays the
//! supervisor: it is user 10001 and has no capability itself. In a plain `cargo test` run every test here is ignored.
//!
//! The paths come from the environment the script sets: `HW_HELPER` (with file capabilities), `HW_HELPER_PLAIN`
//! (without), `HW_WORKER` (the stand-in) and `HW_DATA` (a writable directory on a tmpfs that other users may enter).
#![cfg(target_os = "linux")]

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fd::OwnedFd;
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendFlags, SocketFlags, SocketType, recvmsg, send,
    socketpair,
};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set: run these tests through scripts/web-container-test.sh caps"))
}

/// A fresh home base for one test, so that tests running in parallel never see each other's homes.
fn fresh_base(name: &str) -> PathBuf {
    let top = PathBuf::from(var("HW_DATA")).join(format!("t-{name}"));
    let base = top.join("homes");
    std::fs::create_dir_all(&base).expect("the test directory");
    for dir in [&top, &base] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o711)).expect("mode of the test directory");
    }
    base
}

/// The names in a directory, sorted.
fn listing(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> =
        std::fs::read_dir(dir).expect("a directory").map(|e| e.expect("an entry").file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// The supervisor's side of the control socket and the helper process.
struct Helper {
    child: Child,
    control: Option<OwnedFd>,
    base: PathBuf,
    next_id: u64,
    events: VecDeque<Value>,
}

/// A worker, seen from the supervisor's end of its socket.
struct Worker {
    pid: i32,
    uid: u32,
    reader: BufReader<UnixStream>,
}

impl Helper {
    fn start(name: &str, extra: &[&str]) -> Helper {
        Helper::launch(&var("HW_HELPER"), name, extra, Stdio::inherit())
    }

    fn launch(path: &str, name: &str, extra: &[&str], stderr: Stdio) -> Helper {
        let base = fresh_base(name);
        let (mine, theirs) = socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, SocketFlags::CLOEXEC, None).expect("socket pair");
        let child = Command::new(path)
            .args(["--control-fd", "0", "--worker-path", &var("HW_WORKER"), "--home-base"])
            .arg(&base)
            .args(["--shell", "/bin/sh"])
            .args(extra)
            .stdin(Stdio::from(theirs))
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("the helper starts");
        Helper { child, control: Some(mine), base, next_id: 1, events: VecDeque::new() }
    }

    fn fd(&self) -> &OwnedFd {
        self.control.as_ref().expect("the control socket is open")
    }

    /// One message from the helper, or `None` when nothing arrives in time. Events and answers both come through here.
    fn receive(&self, wait: Duration) -> Option<(Value, Option<OwnedFd>)> {
        let timeout = Timespec { tv_sec: wait.as_secs() as _, tv_nsec: wait.subsec_nanos() as _ };
        let mut fds = [PollFd::new(self.fd(), PollFlags::IN)];
        if poll(&mut fds, Some(&timeout)).expect("poll") == 0 {
            return None;
        }
        let mut buf = vec![0u8; 8192];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let got = recvmsg(self.fd(), &mut [IoSliceMut::new(&mut buf)], &mut ancillary, RecvFlags::CMSG_CLOEXEC).expect("recvmsg");
        let mut passed = None;
        for message in ancillary.drain() {
            if let RecvAncillaryMessage::ScmRights(fds) = message {
                passed = passed.or_else(|| fds.into_iter().next());
            }
        }
        if got.bytes == 0 {
            return None;
        }
        let value: Value = serde_json::from_slice(&buf[..got.bytes]).expect("the helper sends JSON");
        Some((value, passed))
    }

    /// Sends the bytes as one datagram and returns the answer, keeping any events that arrive first.
    fn raw(&mut self, bytes: &[u8]) -> (Value, Option<OwnedFd>) {
        send(self.fd(), bytes, SendFlags::empty()).expect("send");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let (value, fd) = self.receive(left).expect("the helper answers within ten seconds");
            if value.get("event").is_some() {
                self.events.push_back(value);
            } else {
                return (value, fd);
            }
        }
    }

    /// Sends `{"id":N,<members>}` and returns the answer.
    fn request(&mut self, members: &str) -> (Value, Option<OwnedFd>) {
        let id = self.next_id;
        self.next_id += 1;
        let (answer, fd) = self.raw(format!("{{\"id\":{id},{members}}}").as_bytes());
        assert_eq!(answer["id"], id, "the answer echoes the id: {answer}");
        (answer, fd)
    }

    fn spawn(&mut self, uid: u32, mode: &str) -> Result<Worker, Value> {
        let (answer, fd) = self.request(&format!(r#""op":"spawn","uid":{uid},"mode":"{mode}""#));
        if answer["ok"] != true {
            return Err(answer);
        }
        let stream = UnixStream::from(fd.expect("the answer carries the worker's socket"));
        stream.set_read_timeout(Some(Duration::from_secs(10))).expect("read timeout");
        Ok(Worker { pid: answer["pid"].as_i64().expect("a pid") as i32, uid, reader: BufReader::new(stream) })
    }

}

impl Helper {
    /// The number of workers the helper says it holds.
    fn workers(&mut self) -> u64 {
        self.request(r#""op":"ping""#).0["workers"].as_u64().expect("a count")
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.control = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Worker {
    fn send(&mut self, line: &str) {
        writeln!(self.reader.get_mut(), "{line}").expect("write to the stand-in");
    }

    fn line(&mut self) -> String {
        let mut line = String::new();
        self.reader.read_line(&mut line).expect("the stand-in answers");
        line.trim_end().to_owned()
    }

    /// The probe the stand-in sends: its lines up to `probe-end`, keyed by the word before the first colon (the
    /// environment lines keep their `env` key and are collected in `env`).
    fn probe(&mut self) -> Probe {
        let first = self.line();
        assert_eq!(first, "probe-begin", "the stand-in starts with a probe");
        let mut probe = Probe::default();
        loop {
            let line = self.line();
            if line == "probe-end" {
                return probe;
            }
            let (key, value) = line.split_once(':').unwrap_or((line.as_str(), ""));
            let value = value.trim_start_matches([' ', '\t']).to_owned();
            if key == "env" {
                let (name, val) = value.split_once('=').unwrap_or((value.as_str(), ""));
                probe.env.insert(name.to_owned(), val.to_owned());
            } else {
                probe.lines.insert(key.to_owned(), value);
            }
            probe.raw.push(line);
        }
    }
}

#[derive(Default)]
struct Probe {
    lines: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
    raw: Vec<String>,
}

impl Probe {
    fn numbers(&self, key: &str) -> Vec<String> {
        self.lines[key].split_whitespace().map(str::to_owned).collect()
    }
}

#[test]
#[ignore = "needs the container of scripts/web-container-test.sh"]
fn spawns_a_worker_as_the_person_with_nothing_left() {
    let mut helper = Helper::start("spawn-tracer", &[]);
    let mut worker = helper.spawn(20001, "serve").expect("the spawn is accepted");
    let probe = worker.probe();
    for line in &probe.raw {
        if ["Uid", "Gid", "Groups", "CapInh", "CapPrm", "CapEff", "CapAmb", "NoNewPrivs"].iter().any(|k| line.starts_with(k)) {
            println!("probe: {line}");
        }
    }
    assert_eq!(probe.numbers("Uid"), ["20001"; 4], "real, effective, saved and file system user");
    assert_eq!(probe.numbers("Gid"), ["20001"; 4], "real, effective, saved and file system group");
    assert!(probe.lines["Groups"].trim().is_empty(), "no supplementary groups: {:?}", probe.lines["Groups"]);
    for set in ["CapInh", "CapPrm", "CapEff", "CapAmb"] {
        assert_eq!(probe.lines[set], "0000000000000000", "{set} is empty");
    }
    assert_eq!(probe.lines["NoNewPrivs"], "1");
    assert_eq!(worker.pid.to_string(), probe.lines["pid"], "the pid in the answer is the worker");
    assert_eq!(worker.uid, 20001);
}

#[test]
#[ignore = "needs the container of scripts/web-container-test.sh"]
fn fails_closed_without_file_caps() {
    let base = fresh_base("failclosed");
    let (mine, theirs) = socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, SocketFlags::CLOEXEC, None).expect("socket pair");
    let out = Command::new(var("HW_HELPER_PLAIN"))
        .args(["--control-fd", "0", "--worker-path", &var("HW_WORKER"), "--home-base"])
        .arg(&base)
        .stdin(Stdio::from(theirs))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .expect("the helper runs");
    drop(mine);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    println!("fail-closed stderr: {}", stderr.trim_end());
    assert_eq!(out.status.code(), Some(2), "exit status 2: {stderr}");
    for needle in ["CAP_SETUID, CAP_SETGID, CAP_CHOWN, CAP_KILL", "cap_setuid,cap_setgid,cap_chown,cap_kill=p", "container must add"] {
        assert!(stderr.contains(needle), "stderr names {needle:?}: {stderr}");
    }
    assert_eq!(stderr.trim_end().lines().count(), 1, "one sentence: {stderr}");
    assert!(listing(&base).is_empty(), "nothing was created");
}

#[test]
#[ignore = "needs the container of scripts/web-container-test.sh"]
fn refuses_what_the_range_forbids() {
    let mut helper = Helper::start("range", &[]);
    let before = listing(&helper.base);

    // (what is sent after the id, the code the helper must answer with)
    let members: Vec<(String, &str)> = vec![
        (r#""op":"spawn","uid":0,"mode":"serve""#.into(), "uid_refused"),
        (r#""op":"spawn","uid":10001,"mode":"serve""#.into(), "uid_refused"),
        (r#""op":"spawn","uid":1,"mode":"serve""#.into(), "uid_refused"),
        (r#""op":"spawn","uid":999,"mode":"serve""#.into(), "uid_refused"),
        (r#""op":"spawn","uid":1000,"mode":"serve""#.into(), "uid_refused"),
        (r#""op":"spawn","uid":19999,"mode":"serve""#.into(), "uid_refused"),
        (r#""op":"spawn","uid":30000,"mode":"serve""#.into(), "uid_refused"),
        (r#""op":"spawn","uid":65534,"mode":"serve""#.into(), "uid_refused"),
        (r#""op":"spawn","uid":4294967295,"mode":"serve""#.into(), "uid_refused"),
        (r#""op":"spawn","uid":4294967296,"mode":"serve""#.into(), "uid_refused"),
        (r#""op":"spawn","uid":0,"mode":"wipe_home""#.into(), "uid_refused"),
        (r#""op":"spawn","uid":20001,"mode":"shell""#.into(), "mode_refused"),
        (r#""op":"spawn","uid":20001,"mode":"wipe""#.into(), "mode_refused"),
        (r#""op":"spawn","uid":20001,"mode":"SERVE""#.into(), "mode_refused"),
        (r#""op":"exec","uid":20001"#.into(), "unknown_op"),
        (r#""op":"spawn","uid":20001"#.into(), "bad_request"),
        (r#""op":"spawn","mode":"serve""#.into(), "bad_request"),
        (r#""op":"spawn","uid":20001,"mode":"serve","home":"/etc""#.into(), "bad_request"),
        (r#""op":"spawn","uid":"20001","mode":"serve""#.into(), "bad_request"),
        (r#""op":"spawn","uid":-1,"mode":"serve""#.into(), "bad_request"),
        (r#""op":"spawn","uid":20001.5,"mode":"serve""#.into(), "bad_request"),
        (r#""op":"ping","uid":20001"#.into(), "bad_request"),
    ];
    for (members, code) in &members {
        let (answer, fd) = helper.request(members);
        println!("matrix: {members} -> {}", answer["error"]);
        assert_eq!(answer["ok"], false, "{members}: {answer}");
        assert_eq!(answer["error"], *code, "{members}: {answer}");
        assert!(fd.is_none(), "a refusal passes no descriptor: {members}");
    }

    // Messages that are not requests at all. The id comes back when the message has a numeric one.
    let oversize = format!(r#"{{"id":50,"op":"ping","pad":"{}"}}"#, "a".repeat(5000));
    let raws: Vec<(Vec<u8>, &str, u64)> = vec![
        (oversize.into_bytes(), "too_large", 0),
        (br#"{"id":51,"op":"spa"#.to_vec(), "bad_request", 0),
        (b"hello".to_vec(), "bad_request", 0),
        (vec![0xff, 0xfe, 0xfd], "bad_request", 0),
        (br#"{"id":52,"id":53,"op":"ping"}"#.to_vec(), "bad_request", 53),
        (br#"{"op":"ping"}"#.to_vec(), "bad_request", 0),
        (br#"[1,2,3]"#.to_vec(), "bad_request", 0),
        (br#"{"id":"54","op":"ping"}"#.to_vec(), "bad_request", 0),
    ];
    for (bytes, code, id) in &raws {
        let (answer, _) = helper.raw(bytes);
        println!("matrix: {} bytes {:?} -> {}", bytes.len(), String::from_utf8_lossy(&bytes[..bytes.len().min(24)]), answer["error"]);
        assert_eq!(answer["ok"], false, "{answer}");
        assert_eq!(answer["error"], *code, "{answer}");
        assert_eq!(answer["id"], *id, "{answer}");
        let text = answer.to_string();
        assert!(!text.contains("hello") && !text.contains("aaaa"), "a refusal holds no byte of the request: {text}");
    }

    assert_eq!(listing(&helper.base), before, "no directory or file was created");
    assert_eq!(helper.workers(), 0, "no worker was started");
    // The helper is still alive and still serves a good request.
    let mut worker = helper.spawn(20030, "serve").expect("a valid request still works");
    worker.probe();
}

#[test]
#[ignore = "needs the container of scripts/web-container-test.sh"]
fn refuses_a_symlink_as_home() {
    use std::os::unix::fs::MetadataExt;
    let mut helper = Helper::start("symlink", &[]);
    let top = helper.base.parent().expect("the test directory").to_path_buf();

    // A link where a home should be, pointing at a directory somebody else could be tricked into taking over.
    let target = top.join("target");
    std::fs::create_dir(&target).expect("target");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).expect("mode");
    std::fs::write(target.join("precious"), b"keep").expect("file");
    let before = std::fs::metadata(&target).expect("metadata");
    std::os::unix::fs::symlink(&target, helper.base.join("20002")).expect("link");

    let refused = helper.spawn(20002, "serve").err().expect("the link is refused");
    println!("symlink: spawn -> {}", refused["error"]);
    assert_eq!(refused["error"], "home_refused");
    let after = std::fs::metadata(&target).expect("metadata");
    assert_eq!((after.mode(), after.uid(), after.gid()), (before.mode(), before.uid(), before.gid()), "the target is untouched");
    assert_eq!(std::fs::read(target.join("precious")).expect("file"), b"keep");
    assert!(std::fs::symlink_metadata(helper.base.join("20002")).expect("entry").file_type().is_symlink(), "the link is still a link");

    // The other kinds of entry that are not a private directory of the person.
    std::fs::write(helper.base.join("20003"), b"a file").expect("file");
    std::fs::create_dir(helper.base.join("20004")).expect("directory of the supervisor");
    std::fs::set_permissions(helper.base.join("20004"), std::fs::Permissions::from_mode(0o700)).expect("mode");
    for uid in [20003, 20004] {
        let refused = helper.spawn(uid, "serve").err().expect("refused");
        assert_eq!(refused["error"], "home_refused", "uid {uid}");
    }
    assert_eq!(std::fs::metadata(helper.base.join("20004")).expect("metadata").uid(), 10001, "the directory was not given away");
    assert_eq!(helper.workers(), 0);
}

#[test]
#[ignore = "needs the container of scripts/web-container-test.sh"]
fn home_directory_is_private() {
    use std::os::unix::fs::MetadataExt;
    let mut helper = Helper::start("private", &[]);
    let mut worker = helper.spawn(20003, "serve").expect("spawn");
    worker.probe();
    let home = helper.base.join("20003");
    let meta = std::fs::metadata(&home).expect("the home exists");
    println!("home: mode={:o} uid={} gid={}", meta.mode() & 0o7777, meta.uid(), meta.gid());
    assert_eq!(meta.mode() & 0o7777, 0o700);
    assert_eq!((meta.uid(), meta.gid()), (20003, 20003));
    assert_eq!(std::fs::read_dir(&home).err().map(|e| e.kind()), Some(std::io::ErrorKind::PermissionDenied), "the supervisor cannot list it");
    assert!(std::fs::write(home.join("x"), b"x").is_err(), "the supervisor cannot write into it");
}

#[test]
#[ignore = "needs the container of scripts/web-container-test.sh"]
fn a_second_spawn_for_a_live_uid_is_refused() {
    let mut helper = Helper::start("second", &[]);
    let mut first = helper.spawn(20004, "serve").expect("the first spawn");
    first.probe();
    let refused = helper.spawn(20004, "serve").err().expect("the second is refused");
    assert_eq!(refused["error"], "busy_uid");
    assert_eq!(helper.workers(), 1, "still one worker");
    // When the first worker has ended, the person can be served again.
    drop(first);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match helper.spawn(20004, "serve") {
            Ok(mut again) => {
                again.probe();
                break;
            }
            Err(refused) => {
                assert_eq!(refused["error"], "busy_uid");
                assert!(Instant::now() < deadline, "the ended worker is forgotten within ten seconds");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

#[test]
#[ignore = "needs the container of scripts/web-container-test.sh"]
fn the_worker_limit_is_enforced() {
    let mut helper = Helper::start("limit", &["--max-workers", "2"]);
    let mut kept = Vec::new();
    for uid in [20010, 20011] {
        let mut worker = helper.spawn(uid, "serve").expect("within the limit");
        worker.probe();
        kept.push(worker);
    }
    let before = listing(&helper.base);
    let refused = helper.spawn(20012, "serve").err().expect("beyond the limit");
    println!("limit: third spawn -> {}", refused["error"]);
    assert_eq!(refused["error"], "worker_limit");
    assert_eq!(listing(&helper.base), before, "no home for the refused person");
    assert_eq!(helper.workers(), 2);
}

#[test]
#[ignore = "needs the container of scripts/web-container-test.sh"]
fn the_worker_gets_exactly_four_descriptors_and_a_fixed_environment() {
    let mut helper = Helper::start("descriptors", &[]);
    let mut worker = helper.spawn(20005, "serve").expect("spawn");
    let probe = worker.probe();
    println!("descriptors: fds-open: {}", probe.lines["fds-open"]);
    for n in 0..4 {
        println!("descriptors: fd-{n}: {}", probe.lines[&format!("fd-{n}")]);
    }
    for (name, value) in &probe.env {
        println!("environment: {name}={value}");
    }
    assert_eq!(probe.lines["fds-open"], "0 1 2 3", "only the standard streams and the socket cross the exec");
    assert_eq!(probe.lines["fd-0"], "/dev/null");
    assert_eq!(probe.lines["fd-1"], "/dev/null");
    assert!(probe.lines["fd-3"].starts_with("socket:"), "descriptor 3 is the socket: {}", probe.lines["fd-3"]);

    let home = helper.base.join("20005");
    let expected: BTreeMap<String, String> = [
        ("HOME", home.to_string_lossy().into_owned()),
        ("TMPDIR", home.join(".tmp").to_string_lossy().into_owned()),
        ("SHELL", "/bin/sh".to_owned()),
        ("PATH", "/usr/local/bin:/usr/bin:/bin".to_owned()),
        ("LANG", "C.UTF-8".to_owned()),
        ("USER", "u20005".to_owned()),
        ("LOGNAME", "u20005".to_owned()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    assert_eq!(probe.env, expected, "exactly the seven variables, nothing of the helper's or supervisor's environment");

    // Its own process group, and none of the helper's.
    assert_eq!(probe.lines["pgrp"], worker.pid.to_string(), "the worker leads its own process group");
    assert_ne!(probe.lines["pgrp"], helper.child.id().to_string());
}

#[test]
#[ignore = "needs the container of scripts/web-container-test.sh"]
fn a_person_cannot_run_the_helper() {
    let mut helper = Helper::start("runhelper", &[]);
    let mut worker = helper.spawn(20006, "serve").expect("spawn");
    worker.probe();
    for path in [var("HW_HELPER"), var("HW_HELPER_PLAIN")] {
        worker.send(&format!("exec-try {path}"));
        let reply = worker.line();
        println!("exec: {reply}");
        assert!(reply.starts_with("exec-try: status=126"), "the shell could not execute it: {reply}");
        assert!(reply.contains("Permission denied"), "{reply}");
    }
}
