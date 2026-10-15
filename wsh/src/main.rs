//! `wsh`: control the holzWave that runs this terminal.
//!
//! ```text
//! wsh view <file>             show a file in a preview beside this pane
//! wsh edit <file>             open a file in the editor beside this pane
//! wsh setmeta title <text>    name this pane
//! wsh ping                    check that holzWave answers
//! wsh init <bash|zsh|fish>    print the hook that feeds the history; add `eval "$(wsh init bash)"` to the shell's rc file
//! ```
//!
//! It finds the app through three variables holzWave sets in every pane it starts: `HOLZWAVE_IPC` (the folder of the
//! socket), `HOLZWAVE_TOKEN` and `HOLZWAVE_PANE`. Outside a holzWave terminal it says so and does nothing.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use holzwave_ipc::{Cmd, Request};

const USAGE: &str = "usage: wsh view <file> | edit <file> | setmeta title <text> | ping | init <bash|zsh|fish>";

/// The hooks that tell holzWave each command a shell ran. They run only inside a holzWave pane (`HOLZWAVE_IPC` is set), in the
/// background so the prompt never waits, and say nothing when holzWave is not there.
const BASH: &str = r#"[ -n "$HOLZWAVE_IPC" ] || return 0 2>/dev/null
__holzwave_hist() {
  local s=$? c
  c=$(HISTTIMEFORMAT= builtin history 1 | sed 's/^ *[0-9]*[* ] *//')
  if [ -n "$c" ] && [ "$c" != "$__holzwave_last" ]; then
    __holzwave_last=$c
    (wsh hist add "$s" "$PWD" "$c" >/dev/null 2>&1 &)
  fi
  return $s
}
PROMPT_COMMAND="__holzwave_hist${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
"#;
const ZSH: &str = r#"[[ -n "$HOLZWAVE_IPC" ]] || return 0
__holzwave_preexec() { __holzwave_cmd=$1 }
__holzwave_precmd() {
  local s=$?
  if [[ -n "$__holzwave_cmd" ]]; then
    (wsh hist add "$s" "$PWD" "$__holzwave_cmd" >/dev/null 2>&1 &)
    __holzwave_cmd=
  fi
  return $s
}
autoload -Uz add-zsh-hook
add-zsh-hook preexec __holzwave_preexec
add-zsh-hook precmd __holzwave_precmd
"#;
const FISH: &str = r#"test -n "$HOLZWAVE_IPC"; or exit 0
function __holzwave_postexec --on-event fish_postexec
  set -l s $status
  wsh hist add $s $PWD "$argv[1]" >/dev/null 2>&1 &
end
"#;

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
            Ok(Cmd::Hist { cmd: cmd.clone(), cwd: dir.clone(), exit: exit.parse().ok() })
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
            Cmd::Hist { cmd: "ls -la | grep x".into(), cwd: "/home/holz".into(), exit: Some(2) }
        );
        assert_eq!(parse(&args(&["hist", "add", "x", "/", "ls"]), cwd).unwrap(), Cmd::Hist { cmd: "ls".into(), cwd: "/".into(), exit: None });
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
