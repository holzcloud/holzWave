//! `wsh`: control the holzWave that runs this terminal.
//!
//! ```text
//! wsh view <file>             show a file in a preview beside this pane
//! wsh edit <file>             open a file in the editor beside this pane
//! wsh setmeta title <text>    name this pane
//! wsh ping                    check that holzWave answers
//! wsh ai [--] <prompt>...      ask the model of this holzWave; text piped in is sent along; the answer is printed
//! wsh init <bash|zsh|fish>    print the hook that feeds the history and marks where each command's output starts and ends; add `eval "$(wsh init bash)"` to the shell's rc file
//! ```
//!
//! It finds the app through three variables holzWave sets in every pane it starts: `HOLZWAVE_IPC` (the folder of the
//! socket), `HOLZWAVE_TOKEN` and `HOLZWAVE_PANE`. Outside a holzWave terminal it says so and does nothing.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use holzwave_ipc::{Cmd, Request, Response};

mod ai;
mod clean;

const USAGE: &str = "usage: wsh view <file> | edit <file> | setmeta title <text> | ping | ai [--] <prompt>... | init <bash|zsh|fish>";

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

/// Where the app is: the folder of its socket, the token and the pane this terminal is, from the variables holzWave sets in
/// every pane it starts.
fn app_env() -> Result<(PathBuf, String, Option<u64>), (u8, String)> {
    let (Some(dir), Some(token)) = (std::env::var_os("HOLZWAVE_IPC"), std::env::var("HOLZWAVE_TOKEN").ok()) else {
        return Err((1, "wsh: this is not a holzWave terminal (HOLZWAVE_IPC and HOLZWAVE_TOKEN are not set)".into()));
    };
    let pane = std::env::var("HOLZWAVE_PANE").ok().and_then(|p| p.parse().ok());
    Ok((PathBuf::from(dir), token, pane))
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
    let (dir, token, pane) = app_env()?;
    let answer = holzwave_ipc::request(&dir, &Request { token, pane, cmd })
        .map_err(|e| (1, format!("wsh: holzWave does not answer ({e}); is it still running?")))?;
    printable(answer)
}

/// What `wsh` prints of an answer of the app: the message with everything a terminal could act on removed (a file name with
/// an escape character in an error text, for example).
fn printable(answer: Response) -> Result<String, (u8, String)> {
    if answer.ok {
        Ok(clean::text(&answer.message))
    } else {
        Err((1, format!("wsh: {}", clean::text(&answer.message))))
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "ai") {
        return ai::run(&args[1..]);
    }
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
    fn no_hook_ends_the_rc_file_or_touches_the_prompt() {
        for shell in ["bash", "zsh", "fish"] {
            let h = hook(shell).unwrap();
            assert!(h.contains("__holzwave_hook"), "{shell}: an idempotence guard");
            for line in h.lines() {
                let word = line.trim_start();
                let top = line.len() - word.len() <= 2;
                assert!(!(top && (word.starts_with("return") || word.starts_with("exit"))), "{shell}: {line}: ends an rc file");
                for bad in ["|| return", "|| exit", "or return", "or exit"] {
                    assert!(!line.contains(bad), "{shell}: {line}");
                }
                for bad in ["PS1=", "PS2=", "PROMPT=", "RPROMPT=", "set PS1", "function fish_prompt", "functions -c", "trap "] {
                    assert!(!line.contains(bad), "{shell}: {line}: touches the prompt or a trap");
                }
            }
        }
    }

    #[test]
    fn every_hook_marks_four_points_with_a_string_terminator() {
        for shell in ["bash", "zsh", "fish"] {
            let h = hook(shell).unwrap();
            for mark in [r"133;A\033\\", r"133;B\033\\", r"133;C\033\\", r"133;D;%d\033\\"] {
                assert!(h.contains(mark), "{shell}: {mark}");
            }
            for bel in [r"\007", r"\a", r"\x07"] {
                assert!(!h.contains(bel), "{shell}: a mark ends with the string terminator, never BEL");
            }
        }
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

    #[test]
    fn the_messages_of_the_older_commands_are_cleaned() {
        let ok = printable(Response { ok: true, message: "opened \u{1b}]52;c;eA==\u{7}x\u{9b}2J".into() });
        assert_eq!(ok, Ok("opened ]52;c;eA==x2J".to_owned()));
        let refused = printable(Response { ok: false, message: "/tmp/a\u{1b}[31mred\u{7}: no such file".into() });
        assert_eq!(refused, Err((1, "wsh: /tmp/a[31mred: no such file".to_owned())));
        assert_eq!(printable(Response { ok: true, message: String::new() }), Ok(String::new()));
        assert_eq!(printable(Response { ok: true, message: "holzWave\tok\n".into() }), Ok("holzWave\tok\n".to_owned()), "tab and newline stay");
    }
}
