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
use std::io::{BufRead, BufReader, IoSliceMut};
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
        Helper { child, control: Some(mine), next_id: 1, events: VecDeque::new() }
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
