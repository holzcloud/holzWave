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
    use holzwave_spawn_helper::caps::{self, Raised};
    use holzwave_spawn_helper::home::{Existing, HomeBase, HomeError};
    use holzwave_spawn_helper::protocol::{self, Answer, ExitedEvent, MAX_MESSAGE, Mode, Refusal, Request, Signal};
    use holzwave_spawn_helper::spawn::{self, Started, WorkerConfig};
    use holzwave_spawn_helper::sweep;
    use holzwave_spawn_helper::validate::{uid_in_range, worker_file_is_protected};
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::fd::{AsFd, BorrowedFd, OwnedFd};
    use rustix::io::Errno;
    use rustix::net::{
        RecvAncillaryBuffer, RecvFlags, SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketType, recvmsg, sendmsg,
        sockopt,
    };
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::{IoSlice, IoSliceMut};
    use std::mem::MaybeUninit;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::{Child, ExitCode, ExitStatus};
    use std::time::{Duration, Instant};

    /// How long the workers get to end after `SIGTERM` when the control socket has closed.
    const TERMINATION_GRACE: Duration = Duration::from_secs(5);

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
        /// Every user a worker was started for since the helper began: when the helper ends, whatever is left of these
        /// people's processes (a child that outlived its worker, say) ends with it.
        served: BTreeSet<u32>,
        /// Set when a message to the supervisor could not be sent: the supervisor is gone or stuck, and the helper ends.
        broken: Cell<bool>,
    }

    /// A worker that is alive, by the user it runs as.
    struct Worker {
        child: Child,
        /// Used to signal the process without ever naming a process id again, and polled for its end.
        pidfd: OwnedFd,
        pid: i32,
        /// Set for the worker that empties a home on behalf of `remove_home`.
        remove: Option<PendingRemove>,
    }

    /// A `remove_home` request waiting for its wipe worker.
    struct PendingRemove {
        id: u64,
        deadline: Instant,
        answered: bool,
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
            Ok(strays) => {
                return fail(&format!("descriptors {strays:?} are open: the supervisor must pass nothing but the control socket"));
            }
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
        let mut helper = Helper { args, worker_path, own_uid, control, base, workers: BTreeMap::new(), served: BTreeSet::new(), broken: Cell::new(false) };
        helper.serve();
        helper.shutdown();
        ExitCode::SUCCESS
    }

    fn timespec(wait: Duration) -> Timespec {
        Timespec { tv_sec: wait.as_secs() as _, tv_nsec: wait.subsec_nanos() as _ }
    }

    impl Helper {
        /// The event loop: the control socket and one process descriptor per worker. It returns when the supervisor
        /// has closed the control socket or the socket is unusable.
        fn serve(&mut self) {
            while !self.broken.get() {
                let uids: Vec<u32> = self.workers.keys().copied().collect();
                let timeout = self.next_wait().map(timespec);
                let (control_ready, ended) = {
                    let control = self.control.as_fd();
                    let mut fds = Vec::with_capacity(uids.len() + 1);
                    fds.push(PollFd::new(&control, PollFlags::IN));
                    for uid in &uids {
                        fds.push(PollFd::new(&self.workers[uid].pidfd, PollFlags::IN));
                    }
                    match poll(&mut fds, timeout.as_ref()) {
                        Ok(_) => {}
                        Err(Errno::INTR) => continue,
                        Err(_) => return,
                    }
                    let control_ready = !fds[0].revents().is_empty();
                    let ended: Vec<u32> =
                        uids.iter().zip(&fds[1..]).filter(|(_, fd)| !fd.revents().is_empty()).map(|(uid, _)| *uid).collect();
                    (control_ready, ended)
                };
                for uid in ended {
                    self.reap(uid);
                }
                self.check_deadlines();
                if control_ready && !self.read_control() {
                    return;
                }
            }
        }

        /// How long until the next wipe deadline, if any worker is wiping.
        fn next_wait(&self) -> Option<Duration> {
            let now = Instant::now();
            self.workers
                .values()
                .filter_map(|w| w.remove.as_ref().filter(|r| !r.answered))
                .map(|r| r.deadline.saturating_duration_since(now))
                .min()
        }

        /// Reads one message from the control socket and handles it. `false` when the supervisor has closed the socket.
        fn read_control(&mut self) -> bool {
            let mut buf = [0u8; MAX_MESSAGE];
            let received = {
                let mut iov = [IoSliceMut::new(&mut buf)];
                recvmsg(self.control.as_fd(), &mut iov, &mut RecvAncillaryBuffer::default(), RecvFlags::TRUNC)
            };
            let size = match received {
                Ok(msg) => msg.bytes,
                Err(Errno::INTR | Errno::AGAIN) => return true,
                Err(_) => return false,
            };
            // A closed socket reads as an empty message. An empty message sent on purpose reads the same, which is
            // fine: only the supervisor can send one, and it can close the socket anyway.
            if size == 0 {
                return false;
            }
            if size > MAX_MESSAGE {
                self.send(&Answer::refused(0, Refusal::TooLarge).to_bytes(), None);
                return true;
            }
            self.handle(&buf[..size]);
            true
        }

        /// Sends one datagram, with a descriptor if given. A failure marks the control socket as broken.
        fn send(&self, bytes: &[u8], fd: Option<BorrowedFd<'_>>) {
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut control = SendAncillaryBuffer::new(&mut space);
            let fds = [fd.unwrap_or_else(|| self.control.as_fd())];
            if fd.is_some() {
                control.push(SendAncillaryMessage::ScmRights(&fds));
            }
            // Never wait for a supervisor that does not read: a full socket ends the helper like a closed one.
            let flags = SendFlags::NOSIGNAL | SendFlags::DONTWAIT;
            if sendmsg(self.control.as_fd(), &[IoSlice::new(bytes)], &mut control, flags).is_err() {
                self.broken.set(true);
            }
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
                Request::Ping => self.send(&Answer::pong(id, self.workers.len() as u32).to_bytes(), None),
                Request::Spawn { uid, mode } => self.spawn(id, uid, mode),
                Request::KillUid { uid, signal } => self.kill_uid(id, uid, signal),
                Request::RemoveHome { uid } => self.remove_home(id, uid),
            }
        }

        fn person(&self, uid: u64) -> Option<u32> {
            u32::try_from(uid).ok().filter(|uid| uid_in_range(*uid, &self.args.range, self.own_uid))
        }

        /// The checks every start of a worker passes before anything is created.
        fn admit(&self, id: u64, uid: u32) -> bool {
            if self.workers.contains_key(&uid) {
                self.refuse(id, uid, Refusal::BusyUid);
                return false;
            }
            if self.workers.len() >= self.args.max_workers {
                self.refuse(id, uid, Refusal::WorkerLimit);
                return false;
            }
            true
        }

        /// Starts the worker and files it. The supervisor's end of its socket is returned.
        fn start(&mut self, uid: u32, mode: Mode, remove: Option<PendingRemove>) -> Result<OwnedFd, Refusal> {
            let cfg = WorkerConfig {
                worker_path: &self.worker_path,
                shell: &self.args.shell,
                home_base: &self.args.home_base,
                nproc: self.args.nproc,
                nofile: self.args.nofile,
            };
            let Started { child, pidfd, supervisor_end } = spawn::start_worker(&cfg, uid, mode).map_err(|_| Refusal::SpawnFailed)?;
            let pid = child.id() as i32;
            log(if remove.is_some() { "wipe_started" } else { "spawned" }, uid);
            self.served.insert(uid);
            self.workers.insert(uid, Worker { child, pidfd, pid, remove });
            Ok(supervisor_end)
        }

        fn spawn(&mut self, id: u64, uid: u64, mode: Mode) {
            let Some(uid) = self.person(uid) else { return self.refuse(id, 0, Refusal::UidRefused) };
            if !self.admit(id, uid) {
                return;
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
            match self.start(uid, mode, None) {
                Ok(supervisor_end) => {
                    let pid = self.workers[&uid].pid;
                    self.send(&Answer::spawned(id, pid).to_bytes(), Some(supervisor_end.as_fd()));
                }
                Err(refusal) => self.refuse(id, uid, refusal),
            }
        }

        fn kill_uid(&mut self, id: u64, uid: u64, signal: Signal) {
            let Some(uid) = self.person(uid) else { return self.refuse(id, 0, Refusal::UidRefused) };
            let result = Raised::new().map_err(std::io::Error::from).and_then(|_raised| sweep::kill_uid(uid, signal));
            match result {
                Ok(count) => {
                    log("killed", uid);
                    self.send(&Answer::signalled(id, count).to_bytes(), None);
                }
                Err(_) => self.refuse(id, uid, Refusal::KillFailed),
            }
        }

        /// Empties the home with a worker in `wipe_home` mode, which runs as the person and so can remove what the
        /// person owns; then removes the empty directory through the base descriptor. The answer comes when the wipe
        /// worker has ended, or when its time is up.
        fn remove_home(&mut self, id: u64, uid: u64) {
            let Some(uid) = self.person(uid) else { return self.refuse(id, 0, Refusal::UidRefused) };
            if !self.admit(id, uid) {
                return;
            }
            match self.base.inspect(uid) {
                Ok(Existing::Missing) => {
                    log("home_absent", uid);
                    return self.send(&Answer::done(id).to_bytes(), None);
                }
                Ok(Existing::Valid) => {}
                Ok(Existing::Invalid) => return self.refuse(id, uid, Refusal::HomeRefused),
                Err(_) => return self.refuse(id, uid, Refusal::RemoveFailed),
            }
            let pending = PendingRemove { id, deadline: Instant::now() + self.args.wipe_timeout, answered: false };
            // The supervisor's end of the wipe worker's socket is not needed and is closed at once.
            if let Err(refusal) = self.start(uid, Mode::WipeHome, Some(pending)) {
                self.refuse(id, uid, refusal);
            }
        }

        /// Collects a worker whose process descriptor says it has ended, and reports it.
        fn reap(&mut self, uid: u32) {
            let Some(worker) = self.workers.get_mut(&uid) else { return };
            let status = match worker.child.try_wait() {
                Ok(Some(status)) => Some(status),
                Ok(None) => return,
                // Collected by someone else: the exit status is lost, the worker is gone.
                Err(_) => None,
            };
            let Some(worker) = self.workers.remove(&uid) else { return };
            match worker.remove {
                Some(pending) => self.finish_remove(uid, pending, status),
                None => {
                    let (code, signal) = describe(status);
                    log("exited", uid);
                    self.send(&ExitedEvent::new(worker.pid, uid, code, signal).to_bytes(), None);
                }
            }
        }

        fn finish_remove(&self, uid: u32, pending: PendingRemove, status: Option<ExitStatus>) {
            if pending.answered {
                return;
            }
            let answer = if !status.is_some_and(|s| s.success()) {
                Answer::refused(pending.id, Refusal::WipeFailed)
            } else {
                match self.base.remove_empty(uid) {
                    Ok(()) | Err(Errno::NOENT) => Answer::done(pending.id),
                    Err(_) => Answer::refused(pending.id, Refusal::RemoveFailed),
                }
            };
            log(if answer.ok { "home_removed" } else { "home_remove_failed" }, uid);
            self.send(&answer.to_bytes(), None);
        }

        /// A wipe worker that takes too long is killed with everything else of the person's, and the request fails.
        fn check_deadlines(&mut self) {
            let now = Instant::now();
            let late: Vec<u32> = self
                .workers
                .iter()
                .filter(|(_, w)| w.remove.as_ref().is_some_and(|r| !r.answered && now >= r.deadline))
                .map(|(uid, _)| *uid)
                .collect();
            for uid in late {
                if let Ok(_raised) = Raised::new() {
                    let _ = sweep::kill_uid(uid, Signal::Kill);
                }
                if let Some(pending) = self.workers.get_mut(&uid).and_then(|w| w.remove.as_mut()) {
                    pending.answered = true;
                    let id = pending.id;
                    self.refuse(id, uid, Refusal::WipeTimeout);
                }
            }
        }

        /// The supervisor is gone: every worker gets `SIGTERM` and five seconds to end, and then everything that is left
        /// of the processes of the people served is killed.
        fn shutdown(&mut self) {
            let raised = Raised::new().ok();
            for worker in self.workers.values() {
                let _ = sweep::signal_process(&worker.pidfd, Signal::Term);
            }
            let deadline = Instant::now() + TERMINATION_GRACE;
            while !self.workers.is_empty() {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                let uids: Vec<u32> = self.workers.keys().copied().collect();
                let ended: Vec<u32> = {
                    let mut fds: Vec<PollFd<'_>> = uids.iter().map(|uid| PollFd::new(&self.workers[uid].pidfd, PollFlags::IN)).collect();
                    let wait = timespec(left);
                    match poll(&mut fds, Some(&wait)) {
                        Ok(_) | Err(Errno::INTR) => {}
                        Err(_) => break,
                    }
                    uids.iter().zip(&fds).filter(|(_, fd)| !fd.revents().is_empty()).map(|(uid, _)| *uid).collect()
                };
                for uid in ended {
                    if self.workers.get_mut(&uid).is_some_and(|w| matches!(w.child.try_wait(), Ok(None))) {
                        continue;
                    }
                    self.workers.remove(&uid);
                    log("ended_at_shutdown", uid);
                }
            }
            for uid in &self.served {
                if sweep::kill_uid(*uid, Signal::Kill).is_ok_and(|count| count > 0) {
                    log("killed_at_shutdown", *uid);
                }
            }
            for worker in self.workers.values_mut() {
                // The kill has been sent; collect the process so that it does not stay a zombie of the helper.
                let until = Instant::now() + Duration::from_secs(2);
                while Instant::now() < until && matches!(worker.child.try_wait(), Ok(None)) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            drop(raised);
        }
    }

    /// The exit code, or 128 plus the signal, and the signal.
    fn describe(status: Option<ExitStatus>) -> (i32, Option<i32>) {
        match status {
            Some(status) => match (status.code(), status.signal()) {
                (Some(code), _) => (code, None),
                (None, Some(signal)) => (128 + signal, Some(signal)),
                (None, None) => (-1, None),
            },
            None => (-1, None),
        }
    }
}
