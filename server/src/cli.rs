//! The command line of the server: the usage text and the choice of the subcommand.
//!
//! Exit statuses: 0 for success, 1 for a refusal or a failure (one sentence on standard error), 2 for a call the server
//! does not understand.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use crate::accounts::{self, Accounts};
use crate::audit::Audit;
use crate::clock::{Clock, SystemClock};
use crate::config::{self, ENV_KEY_FILE, Env, RealProbe};
use crate::http::health::{self, Health};
use crate::keyfile;
use crate::store::Store;

/// What the server prints when it is called without a known subcommand.
pub const USAGE: &str = "\
usage: holzwave-server <subcommand> [options]

subcommands:
  keygen [--out FILE]   create the server key file (FILE, or the file named by HOLZWAVE_SERVER_KEY_FILE)
  check-config          check the configuration file and the key file, then exit
  bootstrap             print a one-time token that creates the first administrator (only while no account exists)
  healthcheck           ask the running server whether it is ready: prints healthy and exits 0, or one sentence and exits 1
";

/// The exit status for a call the server does not understand.
const USAGE_STATUS: u8 = 2;

/// The exit status for a refusal or a failure.
const FAILURE_STATUS: u8 = 1;

/// The arguments after `worker`: exactly `--mode <serve|wipe_secrets|wipe_home>`.
pub fn parse_worker_args(_args: &[String]) -> Result<crate::worker::runtime::WorkerMode, String> {
    Err("not built".into())
}

/// The `worker` subcommand. `link` is the socket the binary root claimed from descriptor 3, if it was one.
pub fn worker_main(_args: &[String], _link: Option<std::os::unix::net::UnixStream>) -> u8 {
    FAILURE_STATUS
}

/// Prints a problem with the call and the usage, and returns the usage status.
fn usage_error(problem: &str) -> u8 {
    eprintln!("holzwave-server: {problem}");
    eprint!("{USAGE}");
    USAGE_STATUS
}

/// Runs the subcommand named by `args` (the arguments after the program name) and returns the exit status. A call that
/// names no subcommand, or one the server does not have, prints the usage and answers with the usage status.
fn run(args: &[String], env: &Env) -> u8 {
    match args.first().map(String::as_str) {
        Some("keygen") => keygen(&args[1..], env),
        Some("check-config") => check_config(&args[1..], env),
        Some("bootstrap") => bootstrap(&args[1..], env),
        Some("healthcheck") => healthcheck(&args[1..], env),
        Some(name) => usage_error(&format!("unknown subcommand {name:?}")),
        None => {
            eprint!("{USAGE}");
            USAGE_STATUS
        }
    }
}

/// `keygen [--out FILE]`: creates the key file. The only output is the path that was written; the key is never printed.
fn keygen(args: &[String], env: &Env) -> u8 {
    let mut out: Option<String> = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let value = if arg == "--out" {
            match rest.next() {
                Some(value) => value.clone(),
                None => return usage_error("keygen: --out needs a file name"),
            }
        } else if let Some(value) = arg.strip_prefix("--out=") {
            value.to_owned()
        } else {
            return usage_error(&format!("keygen: unknown argument {arg:?}"));
        };
        if out.replace(value).is_some() {
            return usage_error("keygen: --out is given twice");
        }
    }
    let path = match out.or_else(|| env.get(ENV_KEY_FILE).cloned()) {
        Some(path) if !path.is_empty() => path,
        _ => return usage_error(&format!("keygen needs --out FILE or {ENV_KEY_FILE}")),
    };
    match keyfile::generate(Path::new(&path)) {
        Ok(()) => {
            println!("wrote {}", keyfile::shown(Path::new(&path)));
            0
        }
        Err(refusal) => {
            eprintln!("holzwave-server: {refusal}");
            FAILURE_STATUS
        }
    }
}

/// `check-config`: runs the same check as `serve` does before it opens a socket. Prints `configuration ok` and exits 0, or
/// prints the one sentence of the refusal on standard error and exits 1.
fn check_config(args: &[String], env: &Env) -> u8 {
    if let Some(arg) = args.first() {
        return usage_error(&format!("check-config takes no arguments, got {arg:?}"));
    }
    match config::prepare(env, &RealProbe) {
        Ok(_) => {
            println!("configuration ok");
            0
        }
        Err(refusal) => {
            eprintln!("holzwave-server: {refusal}");
            FAILURE_STATUS
        }
    }
}

/// `bootstrap`: makes a new one-time token for the first administrator and prints the one line that announces it. The line is
/// the only output on standard output and the token is nowhere else. While an account exists it refuses with a sentence on
/// standard error and prints no token. An earlier token that was not used stops working.
fn bootstrap(args: &[String], env: &Env) -> u8 {
    if let Some(arg) = args.first() {
        return usage_error(&format!("bootstrap takes no arguments, got {arg:?}"));
    }
    let prepared = match config::prepare(env, &RealProbe) {
        Ok(prepared) => prepared,
        Err(refusal) => {
            eprintln!("holzwave-server: {refusal}");
            return FAILURE_STATUS;
        }
    };
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let opened = Store::open(&prepared.data_dir).map_err(|e| e.to_string()).and_then(|store| {
        let audit = Audit::open(&prepared.data_dir, clock.clone()).map_err(|e| e.to_string())?;
        Ok(Accounts::new(
            Arc::new(store),
            clock.clone(),
            Arc::new(audit),
            prepared.workers.uid_min..=prepared.workers.uid_max,
        ))
    });
    let accounts = match opened {
        Ok(accounts) => accounts,
        Err(sentence) => {
            eprintln!("holzwave-server: {sentence}");
            return FAILURE_STATUS;
        }
    };
    match accounts.bootstrap_issue() {
        Ok(issued) => {
            let line = accounts::announcement(prepared.origin.as_str(), &issued);
            if writeln!(std::io::stdout(), "{line}").is_err() {
                return FAILURE_STATUS;
            }
            0
        }
        Err(refusal) => {
            eprintln!("holzwave-server: {refusal}");
            FAILURE_STATUS
        }
    }
}

/// The one key of the configuration file that `healthcheck` reads. The check is run by a container runtime with a short timeout,
/// so it does not check the whole configuration (that is `check-config`) and does not refuse a file for keys it does not know.
#[derive(serde::Deserialize)]
struct RuntimeDirOnly {
    runtime_dir: Option<std::path::PathBuf>,
}

/// The runtime directory the server was configured with: the key `runtime_dir` of the configuration file, or the default when
/// the file is not there or does not name it.
fn runtime_dir(env: &Env) -> Result<std::path::PathBuf, String> {
    let file = env
        .get(config::ENV_CONFIG)
        .filter(|name| !name.is_empty())
        .map_or_else(|| std::path::PathBuf::from(config::DEFAULT_CONFIG_PATH), std::path::PathBuf::from);
    let default = || std::path::PathBuf::from(config::DEFAULT_RUNTIME_DIR);
    let bytes = match config::Probe::read_config(&RealProbe, &file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(default()),
        Err(error) => return Err(format!("the configuration file {} cannot be read ({error})", file.display())),
    };
    if bytes.len() > config::MAX_CONFIG_BYTES {
        return Err(format!("the configuration file {} is larger than {} bytes", file.display(), config::MAX_CONFIG_BYTES));
    }
    let parsed: RuntimeDirOnly = serde_json::from_slice(&bytes)
        .map_err(|_| format!("the configuration file {} is not valid JSON with an object at the top", file.display()))?;
    match parsed.runtime_dir {
        Some(dir) if !dir.is_absolute() => Err(format!("runtime_dir {} must be an absolute path", dir.display())),
        Some(dir) => Ok(dir),
        None => Ok(default()),
    }
}

/// `healthcheck`: asks the running server on its health socket. Prints `healthy` and exits 0 when it answers that it is ready,
/// otherwise one sentence on standard error and exit status 1. It never takes longer than the probe time.
fn healthcheck(args: &[String], env: &Env) -> u8 {
    if let Some(arg) = args.first() {
        return usage_error(&format!("healthcheck takes no arguments, got {arg:?}"));
    }
    let dir = match runtime_dir(env) {
        Ok(dir) => dir,
        Err(sentence) => {
            eprintln!("holzwave-server: {sentence}");
            return FAILURE_STATUS;
        }
    };
    match health::probe(&dir, health::PROBE_TIMEOUT) {
        Health::Healthy => {
            println!("healthy");
            0
        }
        other => {
            eprintln!("holzwave-server: {}", other.sentence(&dir));
            FAILURE_STATUS
        }
    }
}

/// The variables of the process environment that start with `HOLZWAVE_`; names and values that are not text are left out.
fn process_env() -> Env {
    std::env::vars_os()
        .filter_map(|(name, value)| Some((name.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(name, _)| name.starts_with("HOLZWAVE_"))
        .collect()
}

/// The entry point of the binary: `args` are the arguments after the program name.
pub fn main(args: impl IntoIterator<Item = String>) -> ExitCode {
    let args: Vec<String> = args.into_iter().collect();
    ExitCode::from(run(&args, &process_env()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_subcommand_and_no_subcommand_answer_with_the_usage_status() {
        assert_eq!(run(&["frobnicate".to_owned()], &Env::new()), USAGE_STATUS);
        assert_eq!(run(&[], &Env::new()), USAGE_STATUS);
    }

    #[test]
    fn keygen_without_a_target_or_with_a_bad_option_is_a_usage_error() {
        let none = Env::new();
        let keygen = |args: &[&str]| {
            let mut all = vec!["keygen".to_owned()];
            all.extend(args.iter().map(|a| (*a).to_owned()));
            run(&all, &none)
        };
        assert_eq!(keygen(&[]), USAGE_STATUS);
        assert_eq!(keygen(&["--out"]), USAGE_STATUS);
        assert_eq!(keygen(&["--bogus"]), USAGE_STATUS);
        assert_eq!(keygen(&["--out", "a", "--out", "b"]), USAGE_STATUS);
    }
}
