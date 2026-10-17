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
| zsh_clean | zsh 5 | recorded | `.zprofile` sets PATH; one directory of the list does not exist |
