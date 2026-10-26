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
    use holzwave_spawn_helper::validate::uid_in_range;
    use rustix::fd::{AsFd, BorrowedFd};
    use rustix::io::Errno;
    use rustix::net::{
        RecvAncillaryBuffer, RecvFlags, SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketType, recvmsg, sendmsg,
        sockopt,
    };
    use std::io::{IoSlice, IoSliceMut};
    use std::mem::MaybeUninit;
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
        children: Vec<Child>,
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
        caps::ensure_or_exit();
        let control = std::io::stdin();
        match sockopt::socket_type(control.as_fd()) {
            Ok(SocketType::SEQPACKET) => {}
            _ => return fail("standard input is not a SOCK_SEQPACKET socket: the supervisor must pass the control socket there"),
        }
        let base = match HomeBase::open(&args.home_base, own_uid) {
            Ok(base) => base,
            Err(message) => return fail(&message),
        };
        let worker_path = args.worker_path.clone();
        let mut helper = Helper { args, worker_path, own_uid, control, base, children: Vec::new() };
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

        fn handle(&mut self, bytes: &[u8]) {
            let (id, request) = match protocol::parse(bytes) {
                Ok(parsed) => parsed,
                Err((id, refusal)) => return self.refuse(id, 0, refusal),
            };
            match request {
                Request::Ping => self.send(&Answer::pong(id, self.children.len() as u32).to_bytes(), None),
                Request::Spawn { uid, mode } => self.spawn(id, uid, mode),
                Request::KillUid { .. } | Request::RemoveHome { .. } => self.refuse(id, 0, Refusal::BadRequest),
            }
        }

        fn person(&self, uid: u64) -> Option<u32> {
            u32::try_from(uid).ok().filter(|uid| uid_in_range(*uid, &self.args.range, self.own_uid))
        }

        fn spawn(&mut self, id: u64, uid: u64, mode: Mode) {
            let Some(uid) = self.person(uid) else { return self.refuse(id, 0, Refusal::UidRefused) };
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
                Ok(Started { child, pidfd: _pidfd, supervisor_end }) => {
                    let pid = child.id() as i32;
                    log("spawned", uid);
                    self.send(&Answer::spawned(id, pid).to_bytes(), Some(supervisor_end.as_fd()));
                    self.children.push(child);
                }
                Err(_) => self.refuse(id, uid, Refusal::SpawnFailed),
            }
        }
    }
}
