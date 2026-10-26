//! Starting a worker as a person, and leaving it with nothing.
//!
//! This file holds the only `unsafe` code of the crate: registering the hook that runs in the child between `fork` and
//! `exec`, and adopting descriptor 3 inside it.

use crate::caps::Raised;
use crate::protocol::Mode;
use crate::validate::{home_path, user_name};
use rustix::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use rustix::io::{FdFlags, dup2, fcntl_setfd};
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};
use rustix::process::{DumpableBehavior, Pid, PidfdFlags, Resource, Rlimit, pidfd_open, set_dumpable_behavior, setrlimit};
use rustix::thread::{
    CapabilitySet, CapabilitySets, Gid, Uid, clear_ambient_capability_set, set_capabilities, set_no_new_privs, set_thread_groups,
    set_thread_res_gid, set_thread_res_uid,
};
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// The `PATH` of a worker.
pub const WORKER_PATH_VARIABLE: &str = "/usr/local/bin:/usr/bin:/bin";

/// The `LANG` of a worker.
pub const WORKER_LANG: &str = "C.UTF-8";

/// What every worker is started with; none of it comes from a request.
pub struct WorkerConfig<'a> {
    /// The program, already resolved and checked.
    pub worker_path: &'a Path,
    /// The `SHELL` of the worker.
    pub shell: &'a Path,
    /// The directory that holds the homes.
    pub home_base: &'a Path,
    /// `RLIMIT_NPROC`.
    pub nproc: u64,
    /// `RLIMIT_NOFILE`.
    pub nofile: u64,
}

/// A worker that has been started.
pub struct Started {
    /// The child, kept to collect its exit status.
    pub child: Child,
    /// A descriptor for the process, opened right after the start so that a reused process id can never be signalled.
    pub pidfd: OwnedFd,
    /// The supervisor's end of the worker's socket, to be sent to the supervisor and then closed here.
    pub supervisor_end: OwnedFd,
}

/// Registers the hook that makes the child a plain person. The child is a copy of the helper with only the forking
/// thread, so everything it does here must be a plain system call: no allocation, no lock, no logging.
///
/// The calls, in this order, and why each is allowed between `fork` and `exec`:
///
/// 1. `dup2` of the worker's end of the socket onto descriptor 3 (or clearing close-on-exec if it already is 3): a
///    system call on descriptors the child owns. Nothing else crosses the `exec`, because every other descriptor of the
///    helper is close-on-exec and descriptors 0, 1 and 2 were set by the standard library before this hook.
/// 2. `setrlimit` for `NPROC`, `NOFILE` and `CORE` (soft and hard): lowering limits needs no right and touches only
///    this process. It comes before the change of user so the limits are in place when the new user is checked.
/// 3. `setgroups` with an empty list, then `setresgid`: with the group and supplementary groups of the helper gone the
///    child cannot use any of them. These need the group capability, which the helper has raised while it forks.
/// 4. `setresuid` for real, effective and saved user together, so no id is left to change back to. This needs the user
///    capability, raised the same way.
/// 5. `capset` with empty effective, permitted and inheritable sets, and clearing the ambient set: the change of ids
///    does not drop the capabilities of a process that was not root, so they are removed explicitly.
/// 6. `prctl(PR_SET_NO_NEW_PRIVS)`: set before `exec` so that the new program cannot gain anything from a setuid bit or
///    file capabilities.
/// 7. `prctl(PR_SET_DUMPABLE, 0)`: the child can be inspected by nobody else for the short time before `exec`.
///
/// None of them allocates or takes a lock; the `rustix` wrappers are direct system calls. An error from any of them
/// aborts the start and is reported to the parent by the standard library through its close-on-exec pipe.
#[allow(unsafe_code)]
fn attach_worker_setup(cmd: &mut Command, worker_end: OwnedFd, uid: u32, nproc: u64, nofile: u64) {
    let hook = move || -> io::Result<()> {
        // SAFETY: descriptor 3 is taken over only to be kept open for the program that is about to replace this
        // process. The value is turned back into a raw descriptor below and never closed here. If it is not open yet,
        // `dup2` opens it.
        let mut target = unsafe { OwnedFd::from_raw_fd(3) };
        let moved = if worker_end.as_raw_fd() == 3 { fcntl_setfd(&target, FdFlags::empty()) } else { dup2(&worker_end, &mut target) };
        let _ = target.into_raw_fd();
        moved?;

        for (resource, limit) in [(Resource::Nproc, nproc), (Resource::Nofile, nofile), (Resource::Core, 0)] {
            setrlimit(resource, Rlimit { current: Some(limit), maximum: Some(limit) })?;
        }

        let gid = Gid::from_raw(uid);
        let uid = Uid::from_raw(uid);
        set_thread_groups(&[])?;
        set_thread_res_gid(gid, gid, gid)?;
        set_thread_res_uid(uid, uid, uid)?;

        let none = CapabilitySet::empty();
        set_capabilities(None, CapabilitySets { effective: none, permitted: none, inheritable: none })?;
        clear_ambient_capability_set()?;
        set_no_new_privs(true)?;
        set_dumpable_behavior(DumpableBehavior::NotDumpable)?;
        Ok(())
    };
    // SAFETY: the hook only makes system calls that are safe after `fork` (see the list above).
    unsafe {
        cmd.pre_exec(hook);
    }
}

/// Starts a worker for `uid` in `mode`. The home must exist already. The worker runs as user and group `uid`, with
/// descriptors 0 (`/dev/null`), 1 (`/dev/null`), 2 (inherited) and 3 (its end of a stream socket pair), the seven
/// environment variables below, its own process group, no capability and `no_new_privs`.
pub fn start_worker(cfg: &WorkerConfig<'_>, uid: u32, mode: Mode) -> io::Result<Started> {
    let (supervisor_end, worker_end) = socketpair(AddressFamily::UNIX, SocketType::STREAM, SocketFlags::CLOEXEC, None)?;

    let home = home_path(cfg.home_base, uid);
    let mut cmd = Command::new(cfg.worker_path);
    cmd.arg("worker")
        .arg("--mode")
        .arg(mode.as_str())
        .env_clear()
        .env("HOME", &home)
        .env("TMPDIR", home.join(".tmp"))
        .env("SHELL", cfg.shell)
        .env("PATH", WORKER_PATH_VARIABLE)
        .env("LANG", WORKER_LANG)
        .env("USER", user_name(uid))
        .env("LOGNAME", user_name(uid))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0);
    attach_worker_setup(&mut cmd, worker_end, uid, cfg.nproc, cfg.nofile);

    let child = {
        // The child inherits the raised capabilities at the fork; the guard lowers them again in the helper.
        let _raised = Raised::new()?;
        cmd.spawn()?
    };
    // The hook owns the worker's end of the socket; dropping the command closes the helper's copy.
    drop(cmd);

    let pid = Pid::from_raw(child.id() as i32).ok_or_else(|| io::Error::other("the worker has no process id"))?;
    match pidfd_open(pid, PidfdFlags::empty()) {
        Ok(pidfd) => Ok(Started { child, pidfd, supervisor_end }),
        Err(e) => {
            // The worker is another user's process now: signalling it needs the capability again.
            let mut child = child;
            if let Ok(_raised) = Raised::new() {
                let _ = child.kill();
            }
            let _ = child.wait();
            Err(e.into())
        }
    }
}
