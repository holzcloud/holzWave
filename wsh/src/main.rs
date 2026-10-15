//! `wsh`: control the holzWave that runs this terminal.
//!
//! ```text
//! wsh view <file>             show a file in a preview beside this pane
//! wsh edit <file>             open a file in the editor beside this pane
//! wsh setmeta title <text>    name this pane
//! wsh ping                    check that holzWave answers
//! ```
//!
//! It finds the app through three variables holzWave sets in every pane it starts: `HOLZWAVE_IPC` (the folder of the
//! socket), `HOLZWAVE_TOKEN` and `HOLZWAVE_PANE`. Outside a holzWave terminal it says so and does nothing.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use holzwave_ipc::{Cmd, Request};

const USAGE: &str = "usage: wsh view <file> | edit <file> | setmeta title <text> | ping";

/// The command a command line asks for, with file names made absolute against `cwd` (the app's own directory is not the
/// terminal's).
fn parse(args: &[String], cwd: &Path) -> Result<Cmd, String> {
    let absolute = |name: &str| -> Result<String, String> {
        let path = cwd.join(name);
        path.to_str().map(str::to_owned).ok_or_else(|| "the file name is not valid text".to_owned())
    };
    match args {
        [c, file] if c == "view" => Ok(Cmd::View { path: absolute(file)? }),
        [c, file] if c == "edit" => Ok(Cmd::Edit { path: absolute(file)? }),
        [c, key, rest @ ..] if c == "setmeta" && !rest.is_empty() => Ok(Cmd::Setmeta { key: key.clone(), value: rest.join(" ") }),
        [c] if c == "ping" => Ok(Cmd::Ping),
        _ => Err(USAGE.into()),
    }
}

fn run() -> Result<String, (u8, String)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "help" || args[0] == "--help" || args[0] == "-h" {
        return Err((2, USAGE.into()));
    }
    let cwd = std::env::current_dir().map_err(|e| (1, format!("wsh: no current directory: {e}")))?;
    let cmd = parse(&args, &cwd).map_err(|e| (2, e))?;
    let (Some(dir), Some(token)) = (std::env::var_os("HOLZWAVE_IPC"), std::env::var("HOLZWAVE_TOKEN").ok()) else {
        return Err((1, "wsh: this is not a holzWave terminal (HOLZWAVE_IPC and HOLZWAVE_TOKEN are not set)".into()));
    };
    let pane = std::env::var("HOLZWAVE_PANE").ok().and_then(|p| p.parse().ok());
    let answer = holzwave_ipc::request(&PathBuf::from(dir), &Request { token, pane, cmd })
        .map_err(|e| (1, format!("wsh: holzWave does not answer ({e}); is it still running?")))?;
    if answer.ok { Ok(answer.message) } else { Err((1, format!("wsh: {}", answer.message))) }
}

fn main() -> ExitCode {
    match run() {
        Ok(message) => {
            if !message.is_empty() {
                println!("{message}");
            }
            ExitCode::SUCCESS
        }
        Err((code, message)) => {
            eprintln!("{message}");
            ExitCode::from(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn files_are_made_absolute_against_the_terminals_directory() {
        let cwd = Path::new("/home/holz/proj");
        assert_eq!(parse(&args(&["view", "a.md"]), cwd).unwrap(), Cmd::View { path: "/home/holz/proj/a.md".into() });
        assert_eq!(parse(&args(&["edit", "/etc/hosts"]), cwd).unwrap(), Cmd::Edit { path: "/etc/hosts".into() });
    }

    #[test]
    fn setmeta_joins_the_words_and_everything_else_is_usage() {
        let cwd = Path::new("/");
        assert_eq!(
            parse(&args(&["setmeta", "title", "my", "build"]), cwd).unwrap(),
            Cmd::Setmeta { key: "title".into(), value: "my build".into() }
        );
        assert_eq!(parse(&args(&["ping"]), cwd).unwrap(), Cmd::Ping);
        for bad in [&["view"][..], &["view", "a", "b"], &["setmeta", "title"], &["rm", "x"], &["ping", "x"]] {
            assert_eq!(parse(&args(bad), cwd).unwrap_err(), USAGE, "{bad:?}");
        }
    }
}
