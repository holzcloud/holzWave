//! The worker's run loop: setup, the self-check of its own privileges, and shutdown.

use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

use holzwave_core::secrets::{MemoryStore, SecretStore};
use holzwave_host::local_network::{Source, Unsupported};
use holzwave_host::seams::{Host, HostLimits, Platform};
use holzwave_host::state::{AppCore, ShellSpec};

use crate::wire::{Ctl, Frame, Note, PROTOCOL_VERSION, read_frame};
use crate::worker::dispatch::{self, Item, ServerSink};

/// The name of the worker's data directory inside the home.
pub const DATA_DIR: &str = ".holzwave";

/// What the worker needs to know about its surroundings, and nothing a frame can change.
#[derive(Clone, Debug)]
pub struct WorkerEnv {
    /// The person's home directory. The worker's data directory is `.holzwave` inside it.
    pub home: PathBuf,
    /// The temporary directory.
    pub tmp: PathBuf,
    /// The program a pane runs.
    pub shell: ShellSpec,
}

/// Switches of the integration tests; the product passes the default.
#[derive(Clone, Debug, Default)]
pub struct TestHooks {}

/// How a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEnd {
    /// The link reached end of file.
    LinkClosed,
    /// The supervisor asked for the end.
    Shutdown,
    /// The worker gave up.
    Failed,
}

/// The host of a person's core: the web platform, no local network source, no text scale, the caps of `Init`.
struct WebHost {
    secrets: Arc<dyn SecretStore>,
    limits: HostLimits,
}

impl Host for WebHost {
    fn platform(&self) -> Platform {
        Platform::Web
    }

    fn secrets(&self) -> Arc<dyn SecretStore> {
        self.secrets.clone()
    }

    fn local_network(&self) -> Arc<dyn Source> {
        Arc::new(Unsupported)
    }

    fn text_scale(&self) -> u32 {
        100
    }

    fn limits(&self) -> HostLimits {
        self.limits
    }

    fn local_processes(&self) -> bool {
        false
    }
}

fn cap(value: u32) -> Option<usize> {
    (value != 0).then(|| usize::try_from(value).unwrap_or(usize::MAX))
}

/// Runs the worker on `link` until the link ends.
pub fn run(link: UnixStream, env: WorkerEnv) -> RunEnd {
    run_with(link, env, TestHooks::default())
}

/// [`run`] with the switches of a test.
pub fn run_with(link: UnixStream, env: WorkerEnv, _hooks: TestHooks) -> RunEnd {
    let Ok(write_half) = link.try_clone() else { return RunEnd::Failed };
    let mut reader = link;
    let (out, items) = channel::<Item>();
    let writer = dispatch::spawn_writer(write_half, items);
    let end = serve(&mut reader, &env, &out);
    let _ = out.send(Item::Stop);
    let _ = writer.join();
    end
}

struct Running {
    core: Arc<AppCore>,
    jobs: Sender<dispatch::Job>,
}

fn serve(reader: &mut UnixStream, env: &WorkerEnv, out: &Sender<Item>) -> RunEnd {
    let mut running: Option<Running> = None;
    let end = loop {
        let frame = match read_frame(reader) {
            Ok(Some(frame)) => frame,
            Ok(None) => break RunEnd::LinkClosed,
            Err(_) => break RunEnd::Failed,
        };
        match (frame, running.as_ref()) {
            (Frame::Ctl(Ctl::Init { version, max_panes, max_scrollback_lines }), None) => {
                if version != PROTOCOL_VERSION {
                    break RunEnd::Failed;
                }
                let limits = HostLimits { max_panes: cap(max_panes), max_scrollback_lines: cap(max_scrollback_lines) };
                let Some(core) = build_core(env, out, limits) else { break RunEnd::Failed };
                let (jobs, queue) = channel();
                dispatch::spawn_dispatcher(core.clone(), queue, out.clone());
                running = Some(Running { core, jobs });
                let _ = out.send(Item::Frame(Frame::Note(Note::Ready)));
            }
            (Frame::Cmd(cmd), Some(running)) => match dispatch::accept(&cmd) {
                Ok(job) => {
                    let _ = running.jobs.send(job);
                }
                Err(refusal) => {
                    let _ = out.send(Item::Frame(refusal));
                }
            },
            (Frame::Cmd(cmd), None) => {
                let _ = out.send(Item::Frame(dispatch::refusal(cmd.socket, cmd.request, "the worker is not ready")));
            }
            _ => {}
        }
    };
    if let Some(running) = running {
        running.core.shutdown();
    }
    end
}

fn build_core(env: &WorkerEnv, out: &Sender<Item>, limits: HostLimits) -> Option<Arc<AppCore>> {
    let dir = env.home.join(DATA_DIR);
    if !dir.is_dir() {
        DirBuilder::new().mode(0o700).create(&dir).ok()?;
    }
    let host = WebHost { secrets: Arc::new(MemoryStore::default()), limits };
    let sink = Arc::new(ServerSink::new(out.clone()));
    Some(AppCore::new_with_host(dir, sink, env.shell.clone(), None, None, Arc::new(host)))
}
