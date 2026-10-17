//! `wsh`: control the holzWave that runs this terminal.
//!
//! ```text
//! wsh view <file>             show a file in a preview beside this pane
//! wsh edit <file>             open a file in the editor beside this pane
//! wsh setmeta title <text>    name this pane
//! wsh ping                    check that holzWave answers
//! wsh ai [--] <prompt>...      ask the model of this holzWave; text piped in is sent along; the answer is printed
//! wsh init <bash|zsh|fish>    print the hook that feeds the history and marks where each command's output starts and ends; add `eval "$(wsh init bash)"` to the shell's rc file
//! wsh init <shell> --ctrl-r   also bind Ctrl+R to open the history dialog; plain Ctrl+R stays with the shell without the flag
//! ```
//!
//! `wsh hist add` and `wsh hist open` are for the hooks and the key binding: the first reports a command the shell ran, the
//! second asks the app to open its history search for this pane.
//!
//! It finds the app through three variables holzWave sets in every pane it starts: `HOLZWAVE_IPC` (the folder of the
//! socket), `HOLZWAVE_TOKEN` and `HOLZWAVE_PANE`. Outside a holzWave terminal it says so and does nothing.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use holzwave_ipc::{Cmd, Request, Response};

mod ai;
mod clean;

const USAGE: &str = "usage: wsh view <file> | edit <file> | setmeta title <text> | ping | ai [--] <prompt>... | init <bash|zsh|fish|powershell> [--ctrl-r]";

/// The hooks that tell holzWave each command a shell ran and mark where its output starts and ends. They are files under
/// `hooks/` so the shells and the tests can read the very text that `wsh init` prints. They run only inside a holzWave pane
/// (`HOLZWAVE_IPC` is set), report history in the background so the prompt never waits, and do nothing elsewhere.
const BASH: &str = include_str!("../hooks/bash.sh");
const ZSH: &str = include_str!("../hooks/zsh.zsh");
const FISH: &str = include_str!("../hooks/fish.fish");

/// The binding of Ctrl+R that `wsh init <shell> --ctrl-r` adds after the hook: a file under `hooks/` for each shell, so the
/// shells and the tests read the text that `wsh init` prints. Nothing binds a key unless the user asks for it with the flag.
const BASH_CTRL_R: &str = include_str!("../hooks/bash-ctrl-r.sh");
const ZSH_CTRL_R: &str = include_str!("../hooks/zsh-ctrl-r.zsh");
const FISH_CTRL_R: &str = include_str!("../hooks/fish-ctrl-r.fish");
const POWERSHELL_CTRL_R: &str = include_str!("../hooks/powershell-ctrl-r.ps1");

/// The hook of a shell, or `None`.
fn hook(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(BASH),
        "zsh" => Some(ZSH),
        "fish" => Some(FISH),
        // holzWave starts its own PowerShell hook in its panes, so there is nothing to install without the flag.
        "powershell" => Some(""),
        _ => None,
    }
}

/// The text that binds Ctrl+R in a shell, or `None` when the shell has none.
fn ctrl_r(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(BASH_CTRL_R),
        "zsh" => Some(ZSH_CTRL_R),
        "fish" => Some(FISH_CTRL_R),
        "powershell" => Some(POWERSHELL_CTRL_R),
        _ => None,
    }
}

/// The text `wsh init` prints for the arguments after `init`: the hook of a shell, and with `--ctrl-r` (at most once) the
/// binding of Ctrl+R after it. Anything else is a usage error.
fn init_text(rest: &[String]) -> Result<String, String> {
    let (shell, with_key) = match rest {
        [shell] => (shell, false),
        [shell, flag] if flag == "--ctrl-r" => (shell, true),
        _ => return Err(USAGE.into()),
    };
    let mut text = hook(shell).ok_or_else(|| USAGE.to_owned())?.to_owned();
    if with_key {
        let keys = ctrl_r(shell).ok_or_else(|| USAGE.to_owned())?;
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(keys);
    }
    Ok(text)
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
        [c, sub] if c == "hist" && sub == "open" => Ok(Cmd::HistOpen),
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
    if args[0] == "init" {
        return init_text(&args[1..]).map_err(|e| (2, e));
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
    fn init_prints_the_hook_and_with_the_flag_the_binding_after_it() {
        for shell in ["bash", "zsh", "fish", "powershell"] {
            let plain = init_text(&args(&[shell])).unwrap();
            assert_eq!(plain, hook(shell).unwrap(), "{shell}: without the flag nothing changes");
            let with_key = init_text(&args(&[shell, "--ctrl-r"])).unwrap();
            let mut expect = plain.clone();
            if !expect.is_empty() && !expect.ends_with('\n') {
                expect.push('\n');
            }
            expect.push_str(ctrl_r(shell).unwrap());
            assert_eq!(with_key, expect, "{shell}");
            assert!(with_key.starts_with(&plain) && with_key.len() > plain.len(), "{shell}");
        }
    }

    #[test]
    fn init_refuses_unknown_shells_flags_and_repeats() {
        for bad in [
            &["tcsh"][..],
            &["bash", "--nope"],
            &["bash", "--ctrl-r", "--ctrl-r"],
            &[],
            &["tcsh", "--ctrl-r"],
            &["--ctrl-r"],
            &["powershell", "--ctrl-r", "--ctrl-r"],
        ] {
            assert_eq!(init_text(&args(bad)), Err(USAGE.to_owned()), "{bad:?}");
        }
    }

    #[test]
    fn no_hook_text_without_the_flag_binds_a_key() {
        for shell in ["bash", "zsh", "fish"] {
            let h = hook(shell).unwrap();
            for key in ["bind", r"\C-r", r"\cr", "^R", "Ctrl+r"] {
                assert!(!h.contains(key), "{shell}: the hook binds {key}");
            }
        }
    }

    #[test]
    fn every_binding_runs_only_inside_holzwave_and_asks_in_the_background() {
        for shell in ["bash", "zsh", "fish", "powershell"] {
            let b = ctrl_r(shell).unwrap();
            assert!(b.contains("HOLZWAVE_IPC") && b.contains("hist open"), "{shell}");
            if shell == "powershell" {
                assert!(b.contains("CreateNoWindow") && b.contains("HolzwaveHistory"), "{shell}: started without a window");
            } else {
                assert!(b.contains("wsh hist open") && b.contains("&"), "{shell}: the key never waits for wsh");
                assert!(b.contains("__holzwave_ctrl_r") && b.contains("__holzwave_open_history"), "{shell}: a guard and the function");
            }
            assert!(b.ends_with('\n'), "{shell}: text added after it starts on a line of its own");
        }
    }

    #[test]
    fn no_binding_ends_the_rc_file_or_binds_another_key() {
        // The one spelling of Ctrl+R in each shell's text; no other spelling of any key may appear.
        for (shell, own) in [("bash", r"\C-r"), ("zsh", "^R"), ("fish", r"\cr"), ("powershell", "Ctrl+r")] {
            let b = ctrl_r(shell).unwrap();
            for line in b.lines() {
                let word = line.trim_start();
                let top = line.len() - word.len() <= 2;
                assert!(!(top && (word.starts_with("return") || word.starts_with("exit"))), "{shell}: {line}: ends an rc file");
                for bad in ["|| return", "|| exit", "or return", "or exit", "PS1=", "PROMPT=", "PROMPT_COMMAND", "PS0", "trap ", "READLINE_LINE", "fish_prompt"] {
                    assert!(!line.contains(bad), "{shell}: {line}");
                }
            }
            for other in [r"\C-r", "^R", r"\cr", "Ctrl+r"] {
                assert_eq!(b.contains(other), other == own, "{shell}: the key is spelled {own} and in no other way ({other})");
            }
            let keys = ["\\C-", "bindkey", "bind ", "-Chord"].iter().map(|k| b.matches(k).count()).sum::<usize>();
            assert!(keys >= 1, "{shell}: a key is bound");
            for line in b.lines().filter(|l| ["bind ", "bindkey ", "-Chord"].iter().any(|k| l.contains(k)) && !l.trim_start().starts_with('#')) {
                assert!(line.contains(own), "{shell}: {line}: binds a key other than Ctrl+R");
            }
        }
    }

    #[test]
    fn the_powershell_binding_is_plain_ascii_and_never_evaluates_data() {
        let b = ctrl_r("powershell").unwrap();
        assert!(b.is_ascii(), "plain ASCII");
        for bad in ["Invoke-Expression", "iex ", "http:", "https:"] {
            assert!(!b.contains(bad), "{bad}");
        }
        assert!(!b.split_whitespace().any(|word| word.eq_ignore_ascii_case("-Command")), "no command string is built from data");
        let handlers = b.lines().filter(|l| !l.trim_start().starts_with('#') && l.trim_start().starts_with("try { Set-PSReadLineKeyHandler -")).count();
        assert_eq!(handlers, 2, "the plain and the vi insert call, each in its own try");
        assert_eq!(b.matches("catch {}").count(), 3, "every call that can fail swallows its error");
        assert!(!b.contains("PSConsoleReadLine") && !b.contains("Replace("), "the line buffer is not touched");
    }

    #[test]
    fn init_powershell_without_the_flag_prints_nothing() {
        assert_eq!(init_text(&args(&["powershell"])), Ok(String::new()));
        assert_eq!(init_text(&args(&["powershell", "--ctrl-r"])).unwrap(), POWERSHELL_CTRL_R);
    }

    #[test]
    fn hist_open_takes_no_arguments() {
        let cwd = Path::new("/");
        assert_eq!(parse(&args(&["hist", "open"]), cwd).unwrap(), Cmd::HistOpen);
        for bad in [&["hist", "open", "x"][..], &["hist", "open", "-"], &["hist"], &["hist", "show"]] {
            assert_eq!(parse(&args(bad), cwd).unwrap_err(), USAGE, "{bad:?}");
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
