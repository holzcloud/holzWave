#![deny(unsafe_code)]
//! The binary of the server: hands the command line to the library. The `worker` subcommand is the one that is given a
//! descriptor by its parent, and this is the one place that takes it over.

use std::os::fd::FromRawFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("worker") {
        // Nothing else has run yet: no thread exists and nothing has opened a descriptor of its own.
        let link = claim_link();
        return ExitCode::from(holzwave_server::cli::worker_main(&args[1..], link));
    }
    holzwave_server::cli::main(args)
}

/// Takes over descriptor 3, the socket that the server's helper left for the worker, or returns nothing when it is not one.
///
/// This is the only place of the server that needs the language's escape hatch, and the facts that make it sound are:
///
/// 1. The helper's contract is that it starts the worker with the socket on descriptor 3 and no other descriptor above 2,
///    and that nobody else in the process owns descriptor 3. This function runs first in `main`, before any thread starts,
///    so no code of this process has opened or closed anything since the exec.
/// 2. Before the claim the descriptor is looked at through the standard library: it must exist and be a socket. Anything
///    else (a closed descriptor, a file, a terminal) is not claimed and the worker refuses to serve.
/// 3. It is claimed once: this is the only call of the descriptor number in the program, and the ownership moves into the
///    returned stream, which is closed when it drops.
/// 4. The very next statement marks it close-on-exec, so that no child of the worker (a pane's shell, a helper program) can
///    ever inherit the link.
#[allow(unsafe_code)]
fn claim_link() -> Option<UnixStream> {
    if !std::fs::metadata("/proc/self/fd/3").is_ok_and(|meta| meta.file_type().is_socket()) {
        return None;
    }
    let link = unsafe { UnixStream::from_raw_fd(3) };
    rustix::io::fcntl_setfd(&link, rustix::io::FdFlags::CLOEXEC).ok()?;
    Some(link)
}
