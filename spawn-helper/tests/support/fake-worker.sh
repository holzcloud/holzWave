#!/bin/sh
# Stand-in for the worker program in the container tests of the helper. It is started as
#
#   fake-worker.sh worker --mode <serve|wipe_secrets|wipe_home>
#
# and answers on descriptor 3, the worker's end of the socket pair, one line at a time. In `serve` mode it first sends a
# probe of what the helper left it with, then reads one command per line from descriptor 3 until the socket closes:
#
#   probe              send the probe again
#   exec-try PATH      try to run PATH and report the outcome
#   child              start a child in its own session and report its process id
#   fill DIR           fill the home with files, directories and a link to DIR
#   exit N             exit with status N
#
# The probe is a list of `key: value` lines ending with `probe-end`: the ids, the groups and the capability sets from
# /proc/<pid>/status, the descriptors that survived the exec of this shell (those without close-on-exec), the environment
# exactly as the helper passed it, the process group and the shell's own arguments.

mode=$3

probe() {
    echo "probe-begin"
    while IFS= read -r line; do
        case "$line" in
            Uid:* | Gid:* | Groups:* | CapInh:* | CapPrm:* | CapEff:* | CapBnd:* | CapAmb:* | NoNewPrivs:*) echo "$line" ;;
        esac
    done </proc/$$/status
    open=
    for entry in /proc/$$/fd/*; do
        n=${entry##*/}
        [ -e "/proc/$$/fdinfo/$n" ] || continue
        flags=
        while IFS= read -r line; do
            case "$line" in
                flags:*) flags=${line#flags:} ;;
            esac
        done <"/proc/$$/fdinfo/$n"
        set -- $flags
        flags=${1:-0}
        # O_CLOEXEC is 02000000: a descriptor with it is closed by the next exec and is not part of what a program inherits.
        if [ $((0$flags & 02000000)) -eq 0 ]; then
            open="$open $n"
        fi
    done
    echo "fds-open:$open"
    tr '\0' '\n' </proc/$$/environ | while IFS= read -r line; do echo "env: $line"; done
    echo "pid: $$"
    read -r _ _ _ _ pgrp _ </proc/$$/stat
    echo "pgrp: $pgrp"
    echo "probe-end"
}

case "$mode" in
    wipe_secrets)
        rm -f "$HOME/.holzwave/secrets.store"
        exit 0
        ;;
    wipe_home)
        # Globs, not a path with a trailing slash: a link inside the home is removed itself and never followed.
        rm -rf "$HOME"/* "$HOME"/.[!.]* "$HOME"/..?*
        exit 0
        ;;
    serve) ;;
    *) exit 64 ;;
esac

probe >&3
while IFS= read -r cmd <&3; do
    case "$cmd" in
        probe) probe >&3 ;;
        "exec-try "*)
            path=${cmd#exec-try }
            out=$("$path" 2>&1)
            echo "exec-try: status=$? out=$out" >&3
            ;;
        child)
            setsid sleep 3600 </dev/null >/dev/null 2>&1 &
            echo "child: $!" >&3
            ;;
        "fill "*)
            target=${cmd#fill }
            mkdir -p "$HOME/.holzwave" "$HOME/a/b" "$HOME/.tmp"
            echo secret >"$HOME/.holzwave/secrets.store"
            echo data >"$HOME/a/b/file"
            ln -s "$target" "$HOME/link"
            echo "filled" >&3
            ;;
        "exit "*) exit "${cmd#exit }" ;;
        *) echo "unknown: $cmd" >&3 ;;
    esac
done
exit 0
