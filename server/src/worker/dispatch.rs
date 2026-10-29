//! The dispatch of commands from the page to the application logic of the host, and the sink the core's events leave by.
//!
//! Every frame is resolved with [`lookup`] in the command table of the host crate; this module holds no command name and no
//! list of its own. [`ServerSink`] writes every event of the core as one `Evt` frame with the text the desktop sends.

use std::io::BufWriter;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

use holzwave_host::commands::registry::{Command, Outcome, lookup};
use holzwave_host::state::{AppCore, Out, Sink, Target};
use holzwave_host::windows::{CursorSpot, Place};
use serde_json::Value;

use crate::wire::{CmdFrame, Evt, Frame, Res, ResBody, write_frame};

/// The label of the one window of a person (every browser connection shows it).
pub const WINDOW: &str = "main";

/// What the writer thread is handed.
pub enum Item {
    /// A frame to write.
    Frame(Frame),
    /// Write what is queued and end.
    Stop,
}

/// The sink of the core: events become `Evt` frames for the writer thread.
pub struct ServerSink {
    out: Sender<Item>,
}

impl ServerSink {
    pub fn new(out: Sender<Item>) -> ServerSink {
        ServerSink { out }
    }
}

impl Sink for ServerSink {
    fn send(&self, _target: Target, out: Out) {
        if matches!(out, Out::Exit) {
            return;
        }
        let Ok(text) = serde_json::to_string(&out) else { return };
        let _ = self.out.send(Item::Frame(Frame::Evt(Evt { target: 0, name: out.name().to_owned(), text })));
    }

    fn close_window(&self, _label: &str) {}

    fn open_window(&self, _label: &str, _place: &Place) -> Result<(), String> {
        Err("not available here".into())
    }

    fn cursor_spot(&self, _source: &str) -> CursorSpot {
        CursorSpot::Unknown
    }
}

/// Writes the frames of `items` to `link` until `Stop` or until the link fails.
pub fn spawn_writer(link: UnixStream, items: Receiver<Item>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("hw-writer".into())
        .spawn(move || {
            let mut link = BufWriter::new(link);
            while let Ok(Item::Frame(frame)) = items.recv() {
                if write_frame(&mut link, &frame).is_err() {
                    return;
                }
                if std::io::Write::flush(&mut link).is_err() {
                    return;
                }
            }
        })
        .expect("the writer thread starts")
}

/// A command that is accepted and waits to run.
pub struct Job {
    pub socket: u32,
    pub request: u64,
    pub command: &'static Command,
    pub args: String,
}

/// The answer frame of a refusal: the error value is a JSON string.
pub fn refusal(socket: u32, request: u64, sentence: &str) -> Frame {
    let body = serde_json::to_string(sentence).unwrap_or_else(|_| String::from("\"refused\""));
    Frame::Res(Res { socket, request, body: ResBody::Err(body) })
}

/// Reads the envelope of a command and resolves it. A refusal is the frame to answer with.
pub fn accept(cmd: &CmdFrame) -> Result<Job, Frame> {
    let value: Value =
        serde_json::from_str(&cmd.json).map_err(|_| refusal(cmd.socket, cmd.request, "the command is not valid JSON"))?;
    let name = value.get("cmd").and_then(Value::as_str).unwrap_or("");
    let args = value.get("args").map_or_else(|| String::from("{}"), Value::to_string);
    let command =
        lookup(name).ok_or_else(|| refusal(cmd.socket, cmd.request, "unknown command"))?;
    Ok(Job { socket: cmd.socket, request: cmd.request, command, args })
}

/// Runs a job and returns its answer.
pub fn run_job(core: &AppCore, job: &Job) -> Frame {
    let body = match job.command.call(core, WINDOW, &job.args) {
        Outcome::Ok(text) => ResBody::Ok(text),
        Outcome::Err(text) => ResBody::Err(text),
        Outcome::Binary(bytes) => ResBody::Binary(bytes),
    };
    Frame::Res(Res { socket: job.socket, request: job.request, body })
}

/// Runs the jobs it is sent, one after another, on one thread.
pub fn spawn_dispatcher(core: Arc<AppCore>, jobs: Receiver<Job>, out: Sender<Item>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("hw-dispatch".into())
        .spawn(move || {
            while let Ok(job) = jobs.recv() {
                let _ = out.send(Item::Frame(run_job(&core, &job)));
            }
        })
        .expect("the dispatcher thread starts")
}
