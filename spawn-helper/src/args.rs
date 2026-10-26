//! The command line of the helper, parsed by hand and checked at start.
//!
//! Everything a request must not decide is decided here: the descriptor of the control socket, the worker program, the
//! home base, the range of user ids, the shell and the limits of a worker. A problem is one sentence and exit status 2.

use crate::validate::{DEFAULT_MAX, DEFAULT_MIN, Range};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

/// The checked arguments.
#[derive(Debug, Clone)]
pub struct Args {
    /// The user ids the helper acts for.
    pub range: Range,
    /// The program started for every worker, as given (the helper resolves it before use).
    pub worker_path: PathBuf,
    /// The directory that holds one home per person.
    pub home_base: PathBuf,
    /// `RLIMIT_NPROC` of a worker.
    pub nproc: u64,
    /// `RLIMIT_NOFILE` of a worker.
    pub nofile: u64,
    /// The most workers the helper holds at once.
    pub max_workers: usize,
    /// The `SHELL` of a worker.
    pub shell: PathBuf,
    /// How long a worker that wipes a home may take.
    pub wipe_timeout: Duration,
}

/// The usage line, printed after a problem.
pub const USAGE: &str = "usage: holzwave-spawn-helper --control-fd 0 --worker-path FILE --home-base DIR [--uid-min N] [--uid-max N] \
[--nproc N] [--nofile N] [--max-workers N] [--shell FILE] [--wipe-timeout SECONDS]";

const DEFAULT_NPROC: u64 = 1024;
const DEFAULT_NOFILE: u64 = 4096;
const DEFAULT_MAX_WORKERS: u64 = 256;
const DEFAULT_WIPE_TIMEOUT: u64 = 30;

fn number(flag: &str, value: &str, low: u64, high: u64) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{flag} needs a number"));
    }
    match value.parse::<u64>() {
        Ok(n) if (low..=high).contains(&n) => Ok(n),
        _ => Err(format!("{flag} must be between {low} and {high}")),
    }
}

fn absolute(flag: &str, value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err(format!("{flag} must be an absolute path"));
    }
    Ok(path)
}

/// Parses and checks the arguments (without the program name). `own_uid` is the user the helper runs as.
pub fn parse<I: IntoIterator<Item = OsString>>(args: I, own_uid: u32) -> Result<Args, String> {
    let mut control_fd = None;
    let mut worker_path = None;
    let mut home_base = None;
    let mut uid_min = None;
    let mut uid_max = None;
    let mut nproc = None;
    let mut nofile = None;
    let mut max_workers = None;
    let mut shell = None;
    let mut wipe_timeout = None;

    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let flag = flag.into_string().map_err(|_| "an argument is not valid text".to_owned())?;
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        let value = value.into_string().map_err(|_| format!("the value of {flag} is not valid text"))?;
        let slot = match flag.as_str() {
            "--control-fd" => &mut control_fd,
            "--worker-path" => &mut worker_path,
            "--home-base" => &mut home_base,
            "--uid-min" => &mut uid_min,
            "--uid-max" => &mut uid_max,
            "--nproc" => &mut nproc,
            "--nofile" => &mut nofile,
            "--max-workers" => &mut max_workers,
            "--shell" => &mut shell,
            "--wipe-timeout" => &mut wipe_timeout,
            _ => return Err(format!("unknown argument {flag}")),
        };
        if slot.replace(value).is_some() {
            return Err(format!("{flag} is given twice"));
        }
    }

    // The control socket is the helper's standard input: the supervisor passes it without any unsafe code on its side
    // and the helper adopts no descriptor number it is told.
    let control_fd = control_fd.ok_or("--control-fd is missing")?;
    if number("--control-fd", &control_fd, 0, 1_000_000)? != 0 {
        return Err("--control-fd must be 0: the control socket is the helper's standard input".to_owned());
    }
    let worker_path = absolute("--worker-path", &worker_path.ok_or("--worker-path is missing")?)?;
    let home_base = absolute("--home-base", &home_base.ok_or("--home-base is missing")?)?;
    let min = match uid_min {
        Some(v) => number("--uid-min", &v, 0, u64::from(u32::MAX))? as u32,
        None => DEFAULT_MIN,
    };
    let max = match uid_max {
        Some(v) => number("--uid-max", &v, 0, u64::from(u32::MAX))? as u32,
        None => DEFAULT_MAX,
    };
    let range = Range::new(min, max, own_uid).map_err(str::to_owned)?;
    let nproc = match nproc {
        Some(v) => number("--nproc", &v, 8, 4_194_304)?,
        None => DEFAULT_NPROC,
    };
    let nofile = match nofile {
        Some(v) => number("--nofile", &v, 16, 1_048_576)?,
        None => DEFAULT_NOFILE,
    };
    let max_workers = match max_workers {
        Some(v) => number("--max-workers", &v, 1, 65_536)?,
        None => DEFAULT_MAX_WORKERS,
    } as usize;
    let shell = match shell {
        Some(v) => absolute("--shell", &v)?,
        None if PathBuf::from("/bin/bash").is_file() => PathBuf::from("/bin/bash"),
        None => PathBuf::from("/bin/sh"),
    };
    let wipe_timeout = Duration::from_secs(match wipe_timeout {
        Some(v) => number("--wipe-timeout", &v, 1, 3600)?,
        None => DEFAULT_WIPE_TIMEOUT,
    });

    Ok(Args { range, worker_path, home_base, nproc, nofile, max_workers, shell, wipe_timeout })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(list: &[&str]) -> Result<Args, String> {
        parse(list.iter().map(OsString::from), 10_001)
    }

    const BASE: [&str; 6] = ["--control-fd", "0", "--worker-path", "/opt/w", "--home-base", "/data/homes"];

    #[test]
    fn the_minimum_is_enough_and_takes_the_defaults() {
        let a = run(&BASE).expect("valid");
        assert_eq!((a.range.min(), a.range.max()), (20_000, 29_999));
        assert_eq!((a.nproc, a.nofile, a.max_workers), (1024, 4096, 256));
        assert_eq!(a.wipe_timeout, Duration::from_secs(30));
    }

    #[test]
    fn every_value_can_be_given() {
        let mut v = BASE.to_vec();
        v.extend(["--uid-min", "30000", "--uid-max", "30009", "--nproc", "64", "--nofile", "128", "--max-workers", "3"]);
        v.extend(["--shell", "/bin/sh", "--wipe-timeout", "5"]);
        let a = run(&v).expect("valid");
        assert_eq!((a.range.min(), a.range.max(), a.nproc, a.nofile, a.max_workers), (30_000, 30_009, 64, 128, 3));
        assert_eq!(a.shell, PathBuf::from("/bin/sh"));
        assert_eq!(a.wipe_timeout, Duration::from_secs(5));
    }

    #[test]
    fn refuses_what_is_wrong() {
        let with = |extra: &[&str]| {
            let mut v = BASE.to_vec();
            v.extend_from_slice(extra);
            run(&v)
        };
        assert!(with(&["--uid-min", "999"]).is_err(), "below the floor");
        assert!(with(&["--uid-min", "0", "--uid-max", "5"]).is_err(), "root");
        assert!(with(&["--uid-min", "5000", "--uid-max", "4000"]).is_err(), "inverted");
        assert!(with(&["--uid-min", "10000", "--uid-max", "10002"]).is_err(), "holds the own uid");
        assert!(with(&["--nproc", "abc"]).is_err());
        assert!(with(&["--nproc", "-1"]).is_err());
        assert!(with(&["--nofile", "0"]).is_err());
        assert!(with(&["--max-workers", "0"]).is_err());
        assert!(with(&["--shell", "sh"]).is_err(), "relative shell");
        assert!(with(&["--bogus", "1"]).is_err());
        assert!(with(&["--nproc"]).is_err(), "missing value");
        assert!(with(&["--nproc", "10", "--nproc", "11"]).is_err(), "twice");
    }

    #[test]
    fn requires_the_three_paths_and_a_stdin_control_socket() {
        assert!(run(&[]).is_err());
        assert!(run(&["--worker-path", "/opt/w", "--home-base", "/h"]).is_err(), "no control fd");
        assert!(run(&["--control-fd", "3", "--worker-path", "/opt/w", "--home-base", "/h"]).is_err(), "only 0");
        assert!(run(&["--control-fd", "0", "--worker-path", "w", "--home-base", "/h"]).is_err(), "relative worker");
        assert!(run(&["--control-fd", "0", "--worker-path", "/opt/w", "--home-base", "h"]).is_err(), "relative base");
        assert!(run(&["--control-fd", "0", "--home-base", "/h"]).is_err(), "no worker");
    }
}
