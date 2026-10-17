# Login-shell PATH probe: captures and measurements

An app started from the Finder (or the Dock, or a launcher) gets the system `PATH` and not the one of the user's shell, so
the four command-line tools of the integrations are not found. The app therefore asks the user's login shell once, lazily,
and keeps the directories of the answer. This directory holds what the probe wrote in different situations and the list of
directories that must be kept from it. `scripts/probe-login-path.sh <shell> --check` and the hygiene test
`core/tests/integrate_path_probe_fixtures.rs` read it.

## The probe command

The shell is started as `<shell> -l -c '<script>'` under `env -i` with `HOME`, `USER`, `SHELL` and the launchd `PATH`
(`/usr/bin:/bin:/usr/sbin:/sbin`), with a closed standard input, in its own process group, with a 3 s limit. The script is
one constant string, nothing is interpolated into it:

```
printf '%s\n' @@hw-path-begin; command printenv PATH; printf '%s\n' @@hw-path-end
```

`printenv` and not `echo $PATH`, because fish prints the variable as a list. `command` in front, because a login shell has
the user's aliases and a function or alias named `printenv` must not win. The markers are lines of their own, so that a
banner before them cannot be taken for the value.

## The rules for a capture and its `.expected` file

A capture `<case>.txt` is what the probe wrote to standard output. `<case>.expected` lists the directories the app must keep,
one per line, in this order of rules:

1. Only the text between the markers counts. The last begin marker line wins (a banner may print the same text), the value
   is exactly one line, and a begin marker with no end marker after it gives an empty result.
2. Split the value on `:`.
3. Keep an entry that is an absolute path (starts with `/`) and is a directory that exists. An empty entry, `.`, `./bin` and
   any other relative entry are dropped. The entry is kept as written: a trailing slash is not removed and a space in a
   name is part of the name.
4. Drop an entry that repeats an earlier kept entry (the same text), first one wins.
5. Stop after 64 kept directories.

In the fixtures a directory "exists" unless one of its path components is `missing` followed by digits (`missing1`): the
detector's test models existence by this word and needs no real tree. The recorder checks the model against its scratch tree.

Every capture comes from synthetic startup files in a scratch `HOME` that the recorder removes; the scratch path is replaced
by `/home/user`. A real home directory in any file here fails the hygiene test.

## Captures

| Case | Shell | Source (recorded here or written) | Note |
|---|---|---|---|
| zsh_clean | zsh 5.9 | recorded | `.zprofile` sets PATH; one directory of the list does not exist |
| zsh_banner | zsh 5.9 | recorded | the startup file prints a banner, a prompt-like line, and a fake begin marker with a fake directory; the last begin marker wins |
| zsh_error | zsh 5.9 | recorded | the startup file runs a missing command and prints on standard error (not part of the capture), PATH is set anyway |
| zsh_odd_entries | zsh 5.9 | recorded | `.`, `./bin`, an empty entry, a name with a space, `missing` directories, a duplicate, a trailing slash, 80 more directories: 64 are kept |
| slow_rc | zsh 5.9 | recorded | the startup file sleeps 6 s: the 3 s limit ends the shell, the capture is empty |
| bash_clean | bash 3.2 | recorded | `.bash_profile` sets PATH |
| bash_banner | bash 3.2 | recorded | the same banner as for zsh |
| fish_clean | fish | written | fish is not installed on the recording machine; written from the shape fish gives (PATH exported as one colon-joined string, the same markers); an owner row asks for a real capture |

The recorder is `scripts/probe-login-path.sh <shell> --case <name>`. The startup files set `PATH` completely, so that the
capture does not depend on `/etc/paths` and `path_helper` of the machine that recorded it (on macOS `path_helper` adds the
system directories before the user's file runs; the probe sees them, the fixtures do not hold them).

## Measured numbers

Measured on a Mac (macOS 26, Apple silicon, zsh 5.9, bash 3.2.57), three runs each, milliseconds from the start of the shell
to its exit, under `env -i` with the system `PATH`. A hardware or shell change moves them; the limit of 3 s is far above them.

| Case | Shell | ms (three runs) | Result |
|---|---|---|---|
| clean | zsh | 13, 7, 7 | value found |
| clean | bash | 7, 7, 7 | value found |
| clean | dash | 6, 7, 7 | value found |
| clean | ksh | 15, 7, 7 | value found |
| clean | sh | 6, 7, 13 | value found |
| clean | fish | not measured | fish is not installed here |
| real startup files (the owner's) | zsh | 28, 20, 27 | value found, 15 entries, 10 of them existing directories, no output before the begin marker, nothing on standard error |
| real startup files (the owner's) | bash | 7, 7, 7 | value found, 14 entries, 10 existing, no noise |
| banner | zsh | 7 | value found, the fake begin marker is ignored |
| error | zsh | 13 | value found, two lines on standard error |
| odd entries | zsh | 7 | value found, 64 directories kept of 85 |
| slow startup file (6 s) | zsh | 3000 | limit reached, no output, empty result |
| `exit` in the startup file | zsh | 7 | no output, empty result |
| `exec sleep 6` in the startup file | zsh | 3003 | limit reached, no output, empty result |

The real-file rows hold counts only. Of the four tools, the probed value of the owner's zsh and bash holds the directory of
`kubectl` and of `az`; `talosctl` is not installed here; `gam` is an alias to a program in a directory that is on no `PATH`,
so no probe finds it (the fixed directories and the path field of the entry do).

## What the detector does

- The probe takes at most 3 s. A timeout is not an error: the result is empty and the fixed directories are used.
- Only the text between the markers is used. The last begin marker wins, the value is exactly one line, a begin marker without
  its end marker gives an empty result.
- The filter keeps absolute existing directories only, in order, without duplicates, at most 64. The entry is kept as written.
- `$SHELL` must be an absolute path to a file whose name is one of zsh, bash, fish, sh, dash, ksh. Any other shell (nushell
  among them) is not probed; its user gets the process `PATH` and the fixed directories. `$SHELL` is never run through a shell
  and never interpolated into the script.
- Nothing is probed on Windows: the process `PATH` and the fixed directories are used there.
- A startup file that never returns (an `exec` of a long-running program, a prompt, a sleep) is ended by the limit: the whole
  process group is stopped with TERM, and with KILL after a second. A startup file that ends the shell (`exit`) gives no
  markers, which is an empty result like the timeout.
- The probe never starts an interactive shell, never sources a file by name and never passes text of the environment to a
  shell. Its environment is `HOME`, `USER`, `SHELL` and the system `PATH` only.
