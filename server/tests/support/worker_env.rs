//! The worker driven over a socket pair: the supervisor's side of the link, in a test.
//!
//! `Rig::start()` makes a socket pair and a scratch home, runs `runtime::run` on a thread with the worker end and a fixed
//! shell, and offers `send`, `next`, `wait_res` and `events`. A thread of the rig decodes every frame the worker writes and
//! hands it over a channel, so a test never blocks on the socket itself and every wait has a time limit. The rig stops the
//! worker by closing its end of the pair and joining the thread. Tests pull this file in with
//! `#[path = "support/worker_env.rs"] mod worker_env;` next to `#[path = "support/scratch.rs"] mod scratch;`.
#![allow(dead_code)]

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use holzwave_host::state::ShellSpec;
use holzwave_server::wire::{
    CmdFrame, Ctl, CtlOutcome, Evt, Frame, Note, PROTOCOL_VERSION, Res, ResBody, StoreState, read_frame, write_frame,
};
use holzwave_server::worker::runtime::{RunEnd, TestHooks, WorkerEnv, run_with};
use serde_json::{Value, json};

use crate::scratch::Scratch;

/// How long a test waits for anything the worker should do.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// The picture cap a server has when nothing is configured, in MiB.
pub const DEFAULT_IMAGE_MIB: u32 = 64;

/// The shell command text that prints one picture of `bytes` bytes of RGBA (zeros, 256 pixels wide, so `bytes` must be a
/// multiple of 1024) with the id `id` in one cell, in protocol chunks of 4096 base64 characters, and then sets the title
/// `hw-pic-done`. The title comes after the last chunk in the byte stream, so a test that has seen it knows that the engine
/// has taken or refused the whole picture. Std only: `head`, `base64`, `tr`, `fold` and `printf` of any POSIX system.
pub fn picture_command(bytes: usize, id: u32) -> String {
    assert!(bytes % 1024 == 0 && bytes > 0, "a picture is a whole number of rows of 256 pixels");
    let height = bytes / 1024;
    format!(
        r#"head -c {bytes} /dev/zero | base64 | tr -d '\n' | fold -w 4096 | {{
            read -r first
            printf '\033_Ga=T,f=32,s=256,v={height},i={id},c=1,r=1,m=1,q=2;%s\033\\' "$first"
            while read -r line || [ -n "$line" ]; do printf '\033_Gm=1,q=2;%s\033\\' "$line"; done
            printf '\033_Gm=0,q=2;\033\\'
        }}; printf '\033]0;hw-pic-done\007'"#
    )
}

/// What a rig is started with.
#[derive(Default)]
pub struct RigOptions {
    pub hooks: TestHooks,
    /// The shell a pane runs; `None` is the fixed shell of the rig.
    pub shell: Option<ShellSpec>,
}

/// The supervisor's end of one worker.
pub struct Rig {
    pub home: Scratch,
    /// The file the fixed shell writes its process id to.
    pub pid_file: PathBuf,
    writer: UnixStream,
    frames: Receiver<Result<Frame, String>>,
    seen: Vec<Frame>,
    ended: Receiver<RunEnd>,
    thread: Option<JoinHandle<()>>,
    next_request: u64,
}

impl Rig {
    /// A worker with the fixed shell and no switches.
    pub fn start() -> Rig {
        Rig::start_with(RigOptions::default())
    }

    /// A worker with the given switches. The fixed shell writes its own process id into [`Rig::pid_file`], prints `ready` and
    /// then echoes what it is typed.
    pub fn start_with(options: RigOptions) -> Rig {
        Rig::start_inner(options, true)
    }

    /// A worker whose `TMPDIR` names a directory that is not there, as the spawn helper leaves it: it names `$HOME/.tmp` and
    /// makes nothing.
    pub fn start_without_tmp() -> Rig {
        Rig::start_inner(RigOptions::default(), false)
    }

    fn start_inner(options: RigOptions, make_tmp: bool) -> Rig {
        let home = Scratch::new("worker");
        let tmp = home.join(".tmp");
        if make_tmp {
            std::fs::create_dir(&tmp).expect("the temporary directory");
        }
        let pid_file = home.join("shell.pid");
        let shell = options.shell.unwrap_or_else(|| {
            let script = format!("echo $$ > '{}'; echo ready; exec cat", pid_file.display());
            ShellSpec::new("/bin/sh", &["-c", &script])
        });
        let (ours, theirs) = UnixStream::pair().expect("a socket pair");
        let env = WorkerEnv { home: home.path().to_path_buf(), tmp, shell, user: "tester".into() };
        let hooks = options.hooks;
        let (ended_tx, ended) = channel();
        let thread = std::thread::Builder::new()
            .name("rig-worker".into())
            .spawn(move || {
                let end = run_with(theirs, env, hooks);
                let _ = ended_tx.send(end);
            })
            .expect("the worker thread");
        let mut reading = ours.try_clone().expect("a second handle");
        let (tx, frames) = channel();
        std::thread::Builder::new()
            .name("rig-reader".into())
            .spawn(move || {
                loop {
                    match read_frame(&mut reading) {
                        Ok(Some(frame)) => {
                            if tx.send(Ok(frame)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => return,
                        Err(e) => {
                            let _ = tx.send(Err(e.to_string()));
                            return;
                        }
                    }
                }
            })
            .expect("the reader thread");
        Rig { home, pid_file, writer: ours, frames, seen: Vec::new(), ended, thread: Some(thread), next_request: 1 }
    }

    /// Writes one frame to the worker.
    pub fn send(&mut self, frame: Frame) {
        write_frame(&mut self.writer, &frame).expect("the frame is written to the worker");
    }

    /// Writes raw bytes to the worker, for the frames a well-behaved supervisor never sends.
    pub fn send_raw(&mut self, bytes: &[u8]) {
        use std::io::Write;
        self.writer.write_all(bytes).expect("the bytes are written to the worker");
    }

    /// The next frame the worker wrote, or `None` when none came within `timeout`.
    pub fn next(&mut self, timeout: Duration) -> Option<Frame> {
        match self.frames.recv_timeout(timeout) {
            Ok(Ok(frame)) => {
                self.seen.push(frame.clone());
                Some(frame)
            }
            Ok(Err(e)) => panic!("the worker wrote a frame the rig cannot read: {e}"),
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => None,
        }
    }

    /// Every frame seen so far, in order.
    pub fn seen(&self) -> &[Frame] {
        &self.seen
    }

    /// Reads frames until `wanted` accepts one (or the time is up) and returns it.
    pub fn wait_for(&mut self, what: &str, timeout: Duration, wanted: impl Fn(&Frame) -> bool) -> Frame {
        if let Some(found) = self.seen.iter().find(|f| wanted(f)) {
            return found.clone();
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.next(left) {
                Some(frame) if wanted(&frame) => return frame,
                Some(_) => {}
                None => panic!("waited {timeout:?} for {what}; the worker wrote {:?}", self.seen),
            }
        }
    }

    /// The `Init` handshake: sends `Init` and waits for `Ready`.
    pub fn init(&mut self) {
        self.init_with(0, 0);
    }

    /// [`Rig::init`] with caps (0 is none); the picture cap is the default of a server, 64 MiB.
    pub fn init_with(&mut self, max_panes: u32, max_scrollback_lines: u32) {
        self.init_with_images(max_panes, max_scrollback_lines, DEFAULT_IMAGE_MIB);
    }

    /// [`Rig::init_with`] and the picture cap in whole MiB (0 turns pictures off).
    pub fn init_with_images(&mut self, max_panes: u32, max_scrollback_lines: u32, max_image_mib: u32) {
        self.send(Frame::Ctl(Ctl::Init { version: PROTOCOL_VERSION, max_panes, max_scrollback_lines, max_image_mib }));
        self.wait_for("Ready", PATIENCE, |f| matches!(f, Frame::Note(Note::Ready)));
    }

    /// Sends a command and returns its request id.
    pub fn cmd(&mut self, socket: u32, name: &str, args: Value) -> u64 {
        let request = self.next_request;
        self.next_request += 1;
        self.send_json(socket, request, json!({"cmd": name, "args": args}).to_string());
        request
    }

    /// Sends a command with the JSON text as it is and the request id given.
    pub fn send_json(&mut self, socket: u32, request: u64, json: String) {
        self.send(Frame::Cmd(CmdFrame { socket, request, json }));
    }

    /// The answer to the command `request`.
    pub fn wait_res(&mut self, request: u64) -> Res {
        match self.wait_for(&format!("the answer to request {request}"), PATIENCE, |f| {
            matches!(f, Frame::Res(r) if r.request == request)
        }) {
            Frame::Res(res) => res,
            other => unreachable!("{other:?}"),
        }
    }

    /// Sends a command and waits for its answer.
    pub fn call(&mut self, socket: u32, name: &str, args: Value) -> Res {
        let request = self.cmd(socket, name, args);
        self.wait_res(request)
    }

    /// The text of a successful answer, parsed.
    pub fn ok(&mut self, socket: u32, name: &str, args: Value) -> Value {
        match self.call(socket, name, args).body {
            ResBody::Ok(text) => serde_json::from_str(&text).expect("an answer is JSON"),
            other => panic!("{name} did not succeed: {other:?}"),
        }
    }

    /// Every event named `name` seen so far.
    pub fn events(&self, name: &str) -> Vec<Evt> {
        self.seen
            .iter()
            .filter_map(|f| match f {
                Frame::Evt(e) if e.name == name => Some(e.clone()),
                _ => None,
            })
            .collect()
    }

    /// Waits for an event named `name` whose payload satisfies `wanted`.
    pub fn wait_event(&mut self, name: &str, wanted: impl Fn(&Evt, &Value) -> bool) -> Evt {
        match self.wait_for(&format!("an event {name}"), PATIENCE, |f| match f {
            Frame::Evt(e) if e.name == name => serde_json::from_str::<Value>(&e.text).is_ok_and(|v| wanted(e, &v)),
            _ => false,
        }) {
            Frame::Evt(e) => e,
            other => unreachable!("{other:?}"),
        }
    }

    /// Closes the supervisor's end and waits for the worker thread to return; the end of the run.
    pub fn finish(mut self) -> RunEnd {
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
        let end = self.ended.recv_timeout(PATIENCE).expect("the worker returned in time");
        if let Some(thread) = self.thread.take() {
            thread.join().expect("the worker thread joined");
        }
        end
    }

    /// Sends a control that has a control id and waits for the `CtlDone` note that names it.
    pub fn control(&mut self, ctl: Ctl, id: u64) -> CtlOutcome {
        self.send(Frame::Ctl(ctl));
        match self.wait_for(&format!("the CtlDone of control {id}"), PATIENCE, |f| {
            matches!(f, Frame::Note(Note::CtlDone { ctl, .. }) if *ctl == id)
        }) {
            Frame::Note(Note::CtlDone { outcome, .. }) => outcome,
            other => unreachable!("{other:?}"),
        }
    }

    /// Every store state the worker announced, in order.
    pub fn store_states(&self) -> Vec<StoreState> {
        self.seen
            .iter()
            .filter_map(|f| match f {
                Frame::Note(Note::StoreState(state)) => Some(*state),
                _ => None,
            })
            .collect()
    }

    /// Every pane count the worker announced, in order.
    pub fn pane_counts(&self) -> Vec<u32> {
        self.seen
            .iter()
            .filter_map(|f| match f {
                Frame::Note(Note::PaneCount(n)) => Some(*n),
                _ => None,
            })
            .collect()
    }

    /// How many frames of `pane` seen so far carry a placement list that is not empty.
    pub fn frames_with_images(&self, pane: u64) -> usize {
        self.seen
            .iter()
            .filter_map(|f| match f {
                Frame::Evt(e) if e.name == "frame" => serde_json::from_str::<Value>(&e.text).ok(),
                _ => None,
            })
            .filter(|p| p["pane"] == json!(pane) && p["images"].as_array().is_some_and(|list| !list.is_empty()))
            .count()
    }

    /// The text of an error answer, parsed as JSON.
    pub fn err(&mut self, socket: u32, name: &str, args: Value) -> Value {
        match self.call(socket, name, args).body {
            ResBody::Err(text) => serde_json::from_str(&text).expect("an error value is JSON"),
            other => panic!("{name} did not fail: {other:?}"),
        }
    }

    /// One line for every frame seen: the kind, the ids and the size of the text, never the text itself.
    pub fn describe(&self) -> Vec<String> {
        self.seen
            .iter()
            .map(|frame| match frame {
                Frame::Cmd(c) => format!("Cmd socket {} request {} ({} bytes)", c.socket, c.request, c.json.len()),
                Frame::Res(r) => {
                    let (status, size) = match &r.body {
                        ResBody::Ok(t) => (0, t.len()),
                        ResBody::Err(t) => (1, t.len()),
                        ResBody::Binary(b) => (2, b.len()),
                    };
                    format!("Res socket {} request {} status {status} ({size} bytes)", r.socket, r.request)
                }
                Frame::Evt(e) => format!("Evt {} target {} ({} bytes)", e.name, e.target, e.text.len()),
                other => format!("{other:?}"),
            })
            .collect()
    }

    /// The process id the fixed shell wrote, once it has.
    pub fn shell_pid(&self) -> Option<i32> {
        std::fs::read_to_string(&self.pid_file).ok()?.trim().parse().ok()
    }
}

/// Whether the process `pid` still exists (a zombie that was not reaped counts as existing).
pub fn process_exists(pid: i32) -> bool {
    let Some(pid) = rustix::process::Pid::from_raw(pid) else { return false };
    rustix::process::test_kill_process(pid).is_ok()
}
