//! `wsh`: control the holzWave that runs this terminal.
//!
//! ```text
//! wsh view <file>             show a file in a preview beside this pane
//! wsh edit <file>             open a file in the editor beside this pane
//! wsh setmeta title <text>    name this pane
//! wsh ping                    check that holzWave answers
//! wsh init <bash|zsh|fish>    print the hook that feeds the history and marks where each command's output starts and ends; add `eval "$(wsh init bash)"` to the shell's rc file
//! ```
//!
//! It finds the app through three variables holzWave sets in every pane it starts: `HOLZWAVE_IPC` (the folder of the
//! socket), `HOLZWAVE_TOKEN` and `HOLZWAVE_PANE`. Outside a holzWave terminal it says so and does nothing.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use holzwave_ipc::{Cmd, Request};

const USAGE: &str = "usage: wsh view <file> | edit <file> | setmeta title <text> | ping | init <bash|zsh|fish>";

/// The hooks that tell holzWave each command a shell ran and mark where its output starts and ends. They are files under
/// `hooks/` so the shells and the tests can read the very text that `wsh init` prints. They run only inside a holzWave pane
/// (`HOLZWAVE_IPC` is set), report history in the background so the prompt never waits, and do nothing elsewhere.
const BASH: &str = include_str!("../hooks/bash.sh");
const ZSH: &str = include_str!("../hooks/zsh.zsh");
const FISH: &str = include_str!("../hooks/fish.fish");

/// The hook of a shell, or `None`.
fn hook(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(BASH),
        "zsh" => Some(ZSH),
        "fish" => Some(FISH),
        _ => None,
    }
}

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
        [c, sub, exit, dir, cmd] if c == "hist" && sub == "add" => {
            Ok(Cmd::Hist { line: cmd.clone(), cwd: dir.clone(), exit: exit.parse().ok() })
        }
        _ => Err(USAGE.into()),
    }
}

fn run() -> Result<String, (u8, String)> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "help" || args[0] == "--help" || args[0] == "-h" {
        return Err((2, USAGE.into()));
    }
    if let [c, shell] = args.as_slice() && c == "init" {
        return hook(shell).map(str::to_owned).ok_or((2, USAGE.into()));
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
    fn hist_add_takes_the_status_the_directory_and_the_command_as_they_are() {
        let cwd = Path::new("/");
        assert_eq!(
            parse(&args(&["hist", "add", "2", "/home/holz", "ls -la | grep x"]), cwd).unwrap(),
            Cmd::Hist { line: "ls -la | grep x".into(), cwd: "/home/holz".into(), exit: Some(2) }
        );
        assert_eq!(parse(&args(&["hist", "add", "x", "/", "ls"]), cwd).unwrap(), Cmd::Hist { line: "ls".into(), cwd: "/".into(), exit: None });
        assert!(parse(&args(&["hist", "add", "0", "/"]), cwd).is_err());
    }

    #[test]
    fn every_hook_runs_only_inside_holzwave_and_in_the_background() {
        for shell in ["bash", "zsh", "fish"] {
            let h = hook(shell).unwrap();
            assert!(h.contains("HOLZWAVE_IPC") && h.contains("wsh hist add"), "{shell}");
            assert!(h.contains("&"), "{shell}: the prompt does not wait for wsh");
        }
        assert!(hook("tcsh").is_none());
    }

    #[test]
    fn the_bash_hook_is_guarded_and_marks_four_points() {
        let h = hook("bash").unwrap();
        assert!(h.contains("__holzwave_hook"), "an idempotence guard");
        for mark in [r"133;A\033\\", r"133;B\033\\", r"133;C\033\\", r"133;D;%d\033\\"] {
            assert!(h.contains(mark), "{mark}: a mark ends with the string terminator");
        }
        for line in h.lines() {
            let top = line.len() - line.trim_start().len() <= 2;
            let word = line.trim_start();
            assert!(!(top && (word.starts_with("return") || word.starts_with("exit"))), "{line}: ends an rc file");
            assert!(!line.contains("|| return") && !line.contains("|| exit"), "{line}");
        }
        assert!(!h.contains("PS1"), "the prompt is never read or written");
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
