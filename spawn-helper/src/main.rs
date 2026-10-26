#![deny(unsafe_code)]
//! The privileged helper of the holzWave server: the only process that holds file capabilities (Linux only).
//!
//! Started by the server's supervisor with a `SOCK_SEQPACKET` socket as its standard input. See the library for the
//! rules and `holzwave_spawn_helper::protocol` for the messages.

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("holzwave-spawn-helper: runs on Linux only");
    std::process::ExitCode::from(2)
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    service::run()
}

#[cfg(target_os = "linux")]
mod service {
    use holzwave_spawn_helper::args::{self, Args};
    use holzwave_spawn_helper::caps;
    use holzwave_spawn_helper::home::{Existing, HomeBase, HomeError};
    use holzwave_spawn_helper::protocol::{self, Answer, MAX_MESSAGE, Mode, Refusal, Request};
    use holzwave_spawn_helper::spawn::{self, Started, WorkerConfig};
    use holzwave_spawn_helper::validate::{uid_in_range, worker_file_is_protected};
    use rustix::fd::{AsFd, BorrowedFd};
    use rustix::io::Errno;
    use rustix::net::{
        RecvAncillaryBuffer, RecvFlags, SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketType, recvmsg, sendmsg,
        sockopt,
    };
    use std::io::{IoSlice, IoSliceMut};
    use std::mem::MaybeUninit;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::process::{Child, ExitCode};

    /// One line on standard error: the event name and the user id, nothing else.
    fn log(event: &str, uid: u32) {
        eprintln!("holzwave-spawn-helper: {event} uid={uid}");
    }

    fn fail(message: &str) -> ExitCode {
        eprintln!("holzwave-spawn-helper: {message}");
        ExitCode::from(2)
    }

    struct Helper {
        args: Args,
        worker_path: PathBuf,
        own_uid: u32,
        control: std::io::Stdin,
        base: HomeBase,
        workers: BTreeMap<u32, Worker>,
    }

    /// A worker that is alive, by the user it runs as.
    struct Worker {
        child: Child,
        // Held so that the process can be signalled without ever naming a process id again.
        _pidfd: rustix::fd::OwnedFd,
    }

    pub fn run() -> ExitCode {
        let own_uid = rustix::process::getuid().as_raw();
        let args = match args::parse(std::env::args_os().skip(1), own_uid) {
            Ok(args) => args,
            Err(message) => {
                eprintln!("holzwave-spawn-helper: {message}");
                eprintln!("{}", args::USAGE);
                return ExitCode::from(2);
            }
        };
        match spawn::stray_descriptors() {
            Ok(strays) if strays.is_empty() => {}
            Ok(strays) => return fail(&format!("descriptors {strays:?} are open: the supervisor must pass nothing but the control socket")),
            Err(_) => return fail("/proc/self/fd cannot be read: /proc must be mounted"),
        }
        caps::ensure_or_exit();
        // The worker file is the one thing a compromised supervisor could substitute to run code as another user.
        let mut gids: Vec<u32> = rustix::process::getgroups().unwrap_or_default().iter().map(|g| g.as_raw()).collect();
        gids.push(rustix::process::getgid().as_raw());
        let worker_path = match worker_file_is_protected(&args.worker_path, own_uid, &gids) {
            Ok(real) => real,
            Err(message) => return fail(&format!("the worker file is not protected: {message}")),
        };
        if std::env::set_current_dir("/").is_err() {
            return fail("the helper cannot change to the root directory");
        }
        let control = std::io::stdin();
        match sockopt::socket_type(control.as_fd()) {
            Ok(SocketType::SEQPACKET) => {}
            _ => return fail("standard input is not a SOCK_SEQPACKET socket: the supervisor must pass the control socket there"),
        }
        let base = match HomeBase::open(&args.home_base, own_uid) {
            Ok(base) => base,
            Err(message) => return fail(&message),
        };
        let mut helper = Helper { args, worker_path, own_uid, control, base, workers: BTreeMap::new() };
        helper.serve();
        ExitCode::SUCCESS
    }

    impl Helper {
        fn serve(&mut self) {
            let mut buf = [0u8; MAX_MESSAGE];
            loop {
                let received = {
                    let mut iov = [IoSliceMut::new(&mut buf)];
                    recvmsg(self.control.as_fd(), &mut iov, &mut RecvAncillaryBuffer::default(), RecvFlags::TRUNC)
                };
                let size = match received {
                    Ok(msg) => msg.bytes,
                    Err(Errno::INTR) => continue,
                    Err(_) => return,
                };
                // A closed socket reads as an empty message.
                if size == 0 {
                    return;
                }
                if size > MAX_MESSAGE {
                    self.send(&Answer::refused(0, Refusal::TooLarge).to_bytes(), None);
                    continue;
                }
                self.handle(&buf[..size]);
            }
        }

        fn send(&self, bytes: &[u8], fd: Option<BorrowedFd<'_>>) {
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut control = SendAncillaryBuffer::new(&mut space);
            let fds = [fd.unwrap_or_else(|| self.control.as_fd())];
            if fd.is_some() {
                control.push(SendAncillaryMessage::ScmRights(&fds));
            }
            let _ = sendmsg(self.control.as_fd(), &[IoSlice::new(bytes)], &mut control, SendFlags::NOSIGNAL);
        }

        fn refuse(&self, id: u64, uid: u32, refusal: Refusal) {
            log(&format!("refused_{}", refusal.code()), uid);
            self.send(&Answer::refused(id, refusal).to_bytes(), None);
        }

        /// Forgets the workers that have ended, so that their users can be served again.
        fn collect_exited(&mut self) {
            self.workers.retain(|_, worker| matches!(worker.child.try_wait(), Ok(None)));
        }

        fn handle(&mut self, bytes: &[u8]) {
            self.collect_exited();
            let (id, request) = match protocol::parse(bytes) {
                Ok(parsed) => parsed,
                Err((id, refusal)) => return self.refuse(id, 0, refusal),
            };
            match request {
                Request::Ping => self.send(&Answer::pong(id, self.workers.len() as u32).to_bytes(), None),
                Request::Spawn { uid, mode } => self.spawn(id, uid, mode),
                Request::KillUid { .. } | Request::RemoveHome { .. } => self.refuse(id, 0, Refusal::BadRequest),
            }
        }

        fn person(&self, uid: u64) -> Option<u32> {
            u32::try_from(uid).ok().filter(|uid| uid_in_range(*uid, &self.args.range, self.own_uid))
        }

        fn spawn(&mut self, id: u64, uid: u64, mode: Mode) {
            let Some(uid) = self.person(uid) else { return self.refuse(id, 0, Refusal::UidRefused) };
            if self.workers.contains_key(&uid) {
                return self.refuse(id, uid, Refusal::BusyUid);
            }
            if self.workers.len() >= self.args.max_workers {
                return self.refuse(id, uid, Refusal::WorkerLimit);
            }
            let home = match mode {
                Mode::Serve => self.base.ensure(uid, uid),
                Mode::WipeSecrets | Mode::WipeHome => match self.base.inspect(uid) {
                    Ok(Existing::Valid) => Ok(()),
                    Ok(Existing::Missing) => return self.refuse(id, uid, Refusal::NoHome),
                    Ok(Existing::Invalid) => Err(HomeError::Refused),
                    Err(e) => Err(HomeError::Failed(e)),
                },
            };
            match home {
                Ok(()) => {}
                Err(HomeError::Refused) => return self.refuse(id, uid, Refusal::HomeRefused),
                Err(HomeError::Failed(_)) => return self.refuse(id, uid, Refusal::SpawnFailed),
            }
            let cfg = WorkerConfig {
                worker_path: &self.worker_path,
                shell: &self.args.shell,
                home_base: &self.args.home_base,
                nproc: self.args.nproc,
                nofile: self.args.nofile,
            };
            match spawn::start_worker(&cfg, uid, mode) {
                Ok(Started { child, pidfd, supervisor_end }) => {
                    let pid = child.id() as i32;
                    log("spawned", uid);
                    self.send(&Answer::spawned(id, pid).to_bytes(), Some(supervisor_end.as_fd()));
                    self.workers.insert(uid, Worker { child, _pidfd: pidfd });
                }
                Err(_) => self.refuse(id, uid, Refusal::SpawnFailed),
            }
        }
    }
}
