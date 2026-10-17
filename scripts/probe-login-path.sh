#!/bin/sh
# probe-login-path.sh: the probe that asks the user's login shell for its PATH, with timings and the fixture self-check.
#
# usage: probe-login-path.sh <zsh|bash|fish|sh|dash|ksh> [--synthetic | --case <name> | --try <name> | --real | --check]
#
#   --synthetic   same as `--case clean`
#   --case NAME   build a scratch HOME with a startup file for the case, run the probe in it, print the elapsed
#                 milliseconds and the captured text, and write the capture and its expected list to
#                 core/tests/fixtures/path_probe/. Cases: clean banner error odd_entries slow_rc (fish: clean only).
#   --try NAME    the same, but write nothing (also the cases exit and exec, which have no fixture)
#   --real        run the probe against the real HOME and its startup files, with the launchd PATH. It prints counts
#                 and times only: never a directory, never a line of a startup file. It writes nothing below the repo.
#   --check       feed every capture of the shell through the extraction and the filter and compare with its expected
#                 list; exit 1 and name the case on a difference. Needs no shell but sh and awk.
#
# The probe (a constant, nothing is interpolated into it): the shell is started as `<shell> -l -c '<script>'` under
# `env -i` (HOME, USER, SHELL and the launchd PATH only) with a closed standard input and a hard time limit. The script
# prints a begin marker line, runs `command printenv PATH` and prints an end marker line. printenv, not `echo $PATH`:
# fish prints the variable as a list. `command` so that an alias of the same name cannot win.
#
# The extraction: only the text between the markers counts. The last begin marker wins (a banner may print the same
# text), the value is exactly one line, and a begin marker without its end marker gives an empty result.
# The filter: split on `:`, keep absolute directories that exist, in order, duplicates dropped, at most 64. In the
# fixtures "exists" is modelled by the name: a directory with a path component `missing<digits>` does not exist.
#
# Plain POSIX sh, no bash features. Reads shell startup files, never changes one.

set -u

LAUNCHD_PATH=/usr/bin:/bin:/usr/sbin:/sbin
LIMIT=3
MAX_DIRS=64
BEGIN='@@hw-path-begin'
END='@@hw-path-end'
# The probe script. A constant: it is never built from the environment or from an argument.
PROBE_SCRIPT="printf '%s\\n' @@hw-path-begin; command printenv PATH; printf '%s\\n' @@hw-path-end"

ROOT=$(cd "$(dirname "$0")/.." && pwd)
FIX="$ROOT/core/tests/fixtures/path_probe"

WORK=""

usage() {
    echo "usage: $0 <zsh|bash|fish|sh|dash|ksh> [--synthetic | --case <name> | --try <name> | --real | --check]" >&2
    exit 2
}

die() {
    echo "probe-login-path: $*" >&2
    exit 1
}

cleanup() {
    if [ -n "$WORK" ] && [ -d "$WORK" ]; then
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# The absolute path of a known shell, or nothing. Fixed system directories first, then the lookup of the name.
shell_path() {
    for _d in /bin /usr/bin; do
        if [ -x "$_d/$1" ] && [ -f "$_d/$1" ]; then
            echo "$_d/$1"
            return 0
        fi
    done
    _p=$(command -v "$1" 2>/dev/null) || return 1
    case "$_p" in
        /*) [ -x "$_p" ] && [ -f "$_p" ] && { echo "$_p"; return 0; } ;;
    esac
    return 1
}

# The runner: starts the command in its own process group with a closed standard input, waits at most LIMIT seconds,
# and ends the whole group (TERM, then KILL after a second) when the limit passes or when the command has returned, so
# that no shell, sleep or background child is left behind. Job control of sh is not used: dash has none without a tty.
# It prints "<elapsed ms> <timed out 0|1>". The command is run without a shell, as an argument list.
RUNNER='
use strict;
use POSIX ();
use Time::HiRes qw(time sleep);
my ($limit, $out, $err, @cmd) = @ARGV;
my $t0 = time;
my $pid = fork();
die "fork failed" unless defined $pid;
if (!$pid) {
    POSIX::setpgid(0, 0);
    open(STDIN, "<", "/dev/null");
    open(STDOUT, ">", $out);
    open(STDERR, ">", $err);
    exec { $cmd[0] } @cmd;
    POSIX::_exit(127);
}
my $timed = 0;
while (1) {
    last if waitpid($pid, POSIX::WNOHANG()) == $pid;
    if (time - $t0 >= $limit) { $timed = 1; last; }
    sleep(0.005);
}
my $elapsed = int((time - $t0) * 1000);
if ($timed) {
    kill "TERM", -$pid;
    my $t1 = time;
    while (time - $t1 < 1) { last if waitpid($pid, POSIX::WNOHANG()) == $pid; sleep(0.005); }
}
kill "KILL", -$pid;
waitpid($pid, 0);
printf "%d %d\n", $elapsed, $timed;
'

# run_probe SHELL HOME USER OUT ERR: one probe. Sets ELAPSED_MS and TIMED_OUT (0 or 1).
run_probe() {
    command -v perl >/dev/null 2>&1 || die "perl is needed for the time limit and the process group"
    _res=$(perl -e "$RUNNER" "$LIMIT" "$4" "$5" /usr/bin/env -i HOME="$2" USER="$3" SHELL="$1" PATH="$LAUNCHD_PATH" \
        "$1" -l -c "$PROBE_SCRIPT") || die "the runner failed"
    ELAPSED_MS=${_res%% *}
    TIMED_OUT=${_res##* }
}

# extract: standard input is a capture, standard output is the PATH value (nothing when there is none).
extract() {
    awk -v b="$BEGIN" -v e="$END" '
        $0 == b { state = 1; n = 0; value = ""; have = 0; next }
        state == 1 && $0 == e { if (n == 1) { result = value; have = 1 } state = 0; next }
        state == 1 { n++; value = $0; next }
        END { if (have) print result }'
}

# filter: standard input is the PATH value, standard output the directories to keep, one per line. Existence is
# modelled by the name: a path component `missing<digits>` is a directory that does not exist.
filter() {
    awk -F: -v max="$MAX_DIRS" '
        {
            kept = 0
            for (i = 1; i <= NF; i++) {
                d = $i
                if (substr(d, 1, 1) != "/") continue
                if (d ~ /(^|\/)missing[0-9]+(\/|$)/) continue
                if (d in seen) continue
                seen[d] = 1
                print d
                if (++kept >= max) break
            }
        }'
}

# ---- the startup files of the synthetic cases -----------------------------------------------------------------

BASE_PATH='$HOME/bin:$HOME/.local/bin:$HOME/missing1:$HOME/tools/bin:/usr/bin:/bin:/usr/sbin:/sbin'

odd_path() {
    _p='.:./bin::$HOME/with space/bin:$HOME/missing1:$HOME/bin:$HOME/bin:$HOME/opt/bin/'
    _i=1
    while [ "$_i" -le 80 ]; do
        _p="$_p:\$HOME/extra/d$(printf '%02d' "$_i")"
        _i=$((_i + 1))
    done
    printf '%s' "$_p:\$HOME/missing2:/usr/bin:/bin"
}

# The directories of the scratch tree that exist. Everything named missing<digits> is never created.
make_tree() {
    mkdir -p "$1/bin" "$1/.local/bin" "$1/tools/bin" "$1/with space/bin" "$1/opt/bin" || die "cannot build the scratch tree"
    _i=1
    while [ "$_i" -le 80 ]; do
        mkdir -p "$1/extra/d$(printf '%02d' "$_i")"
        _i=$((_i + 1))
    done
}

# rc_file SHELL: the login startup file the shell reads below HOME.
rc_file() {
    case "$1" in
        zsh) echo ".zprofile" ;;
        bash) echo ".bash_profile" ;;
        fish) echo ".config/fish/config.fish" ;;
        *) echo ".profile" ;;
    esac
}

# write_rc SHELL CASE HOME: write the startup file of a case. The text is data of this script; the shell that runs it
# is the one under test.
write_rc() {
    _sh=$1
    _case=$2
    _home=$3
    _file="$_home/$(rc_file "$_sh")"
    mkdir -p "$(dirname "$_file")"
    if [ "$_sh" = fish ]; then
        [ "$_case" = clean ] || die "fish has the case clean only"
        printf '%s\n' 'set -gx PATH $HOME/bin $HOME/.local/bin $HOME/missing1 $HOME/tools/bin /usr/bin /bin /usr/sbin /sbin' >"$_file"
        return 0
    fi
    {
        case "$_case" in
            clean)
                printf '%s\n' "PATH=\"$BASE_PATH\"" 'export PATH'
                ;;
            banner)
                # A banner and a prompt-like line on standard output, one of them the begin marker with a fake value.
                printf '%s\n' "printf 'Welcome back!\\n'"
                printf '%s\n' "printf 'Last login: Mon Jan  1 00:00:00 on ttys000\\n'"
                printf '%s\n' "printf '%s\\n' $BEGIN"
                printf '%s\n' "printf '%s\\n' /injected/bin"
                printf '%s\n' "PATH=\"$BASE_PATH\"" 'export PATH'
                printf '%s\n' "printf 'user@host ~ %% \\n'"
                ;;
            error)
                # A command that does not exist and a message on standard error; PATH is set anyway.
                printf '%s\n' 'hw_no_such_command_xyz --flag'
                printf '%s\n' "printf 'startup: something failed\\n' >&2"
                printf '%s\n' "PATH=\"$BASE_PATH\"" 'export PATH'
                ;;
            odd_entries)
                printf '%s\n' "PATH=\"$(odd_path)\"" 'export PATH'
                ;;
            slow_rc)
                printf '%s\n' 'sleep 6' "PATH=\"$BASE_PATH\"" 'export PATH'
                ;;
            exit)
                printf '%s\n' "PATH=\"$BASE_PATH\"" 'export PATH' 'exit 0'
                ;;
            exec)
                printf '%s\n' 'exec sleep 6'
                ;;
            *) die "unknown case $_case" ;;
        esac
    } >"$_file"
}

# The name of the fixture of a case, or nothing when the case has none.
fixture_name() {
    case "$2" in
        slow_rc) [ "$1" = zsh ] && echo slow_rc ;;
        clean | banner | error | odd_entries) echo "$1_$2" ;;
    esac
}

escape_sed() {
    printf '%s' "$1" | sed 's/[][\.*^$|/]/\\&/g'
}

# run_case SHELL CASE WRITE: the synthetic probe of one case; WRITE is 1 to write the fixture files.
run_case() {
    _sh=$1
    _case=$2
    _write=$3
    _path=$(shell_path "$_sh") || {
        echo "skipped $_sh"
        return 0
    }
    WORK=$(mktemp -d "${TMPDIR:-/tmp}/hw-probe.XXXXXX") || die "mktemp failed"
    _home="$WORK/home"
    mkdir "$_home"
    _phys=$(cd -P "$_home" && pwd)
    make_tree "$_home"
    write_rc "$_sh" "$_case" "$_home"
    run_probe "$_path" "$_home" user "$WORK/out" "$WORK/err"
    # The scratch path becomes /home/user, the physical path first.
    sed "s/$(escape_sed "$_phys")/\\/home\\/user/g; s/$(escape_sed "$_home")/\\/home\\/user/g" "$WORK/out" >"$WORK/capture"
    extract <"$WORK/capture" | filter >"$WORK/expected"
    # The name model must agree with the real scratch tree: every kept directory below the stand-in exists.
    while IFS= read -r _d; do
        case "$_d" in
            /home/user | /home/user/*)
                [ -d "$_phys${_d#/home/user}" ] || die "the filter kept $_d, which does not exist in the scratch tree"
                ;;
        esac
    done <"$WORK/expected"
    echo "shell=$_sh case=$_case elapsed_ms=$ELAPSED_MS timed_out=$TIMED_OUT stderr_lines=$(wc -l <"$WORK/err" | tr -d ' ')"
    echo "--- capture (standard output)"
    cat "$WORK/capture"
    echo "--- expected ($(wc -l <"$WORK/expected" | tr -d ' ') directories)"
    if [ "$_write" = 1 ]; then
        _name=$(fixture_name "$_sh" "$_case")
        [ -n "$_name" ] || die "the case $_case of $_sh has no fixture name (use --try)"
        mkdir -p "$FIX"
        cp "$WORK/capture" "$FIX/$_name.txt"
        cp "$WORK/expected" "$FIX/$_name.expected"
        echo "wrote $_name.txt and $_name.expected"
    fi
    rm -rf "$WORK"
    WORK=""
}

# ---- the real startup files: counts and times, nothing else ------------------------------------------------------

run_real() {
    _sh=$1
    _path=$(shell_path "$_sh") || {
        echo "skipped $_sh"
        return 0
    }
    WORK=$(mktemp -d "${TMPDIR:-/tmp}/hw-probe.XXXXXX") || die "mktemp failed"
    run_probe "$_path" "$HOME" "${USER:-user}" "$WORK/out" "$WORK/err"
    extract <"$WORK/out" >"$WORK/value"
    _entries=$(awk -F: 'NF { print NF }' "$WORK/value")
    awk -F: '{ for (i = 1; i <= NF; i++) if (substr($i, 1, 1) == "/" && !($i in s)) { s[$i] = 1; print $i } }' "$WORK/value" >"$WORK/abs"
    : >"$WORK/kept"
    while IFS= read -r _d; do
        [ -d "$_d" ] && printf '%s\n' "$_d" >>"$WORK/kept"
    done <"$WORK/abs"
    _kept=$(wc -l <"$WORK/kept" | tr -d ' ')
    _before=$(awk -v b="$BEGIN" '$0 == b { n = NR - 1; seen = 1 } END { if (seen) print n; else print "none" }' "$WORK/out")
    _noise=no
    case "$_before" in
        none) _noise="no-markers" ;;
        0) _noise=no ;;
        *) _noise=yes ;;
    esac
    echo "shell=$_sh real elapsed_ms=$ELAPSED_MS timed_out=$TIMED_OUT entries=${_entries:-0} kept_existing=$_kept"
    echo "noise: $_noise stdout_lines_before_begin=$_before stderr_lines=$(wc -l <"$WORK/err" | tr -d ' ')"
    for _tool in kubectl az gam talosctl; do
        _hit=missing
        while IFS= read -r _d; do
            if [ -f "$_d/$_tool" ] && [ -x "$_d/$_tool" ]; then
                _hit=found
                break
            fi
        done <"$WORK/kept"
        echo "tool $_tool: $_hit in the probed PATH"
    done
    rm -rf "$WORK"
    WORK=""
}

# ---- the self-check against the fixtures ------------------------------------------------------------------------

run_check() {
    _sh=$1
    _status=0
    _n=0
    WORK=$(mktemp -d "${TMPDIR:-/tmp}/hw-probe.XXXXXX") || die "mktemp failed"
    for _f in "$FIX/${_sh}_"*.txt "$FIX/slow_rc.txt"; do
        [ -f "$_f" ] || continue
        case "$_f" in
            */slow_rc.txt) [ "$_sh" = zsh ] || continue ;;
        esac
        _case=$(basename "$_f" .txt)
        _exp="$FIX/$_case.expected"
        if [ ! -f "$_exp" ]; then
            echo "FAILED $_case: no $_case.expected"
            _status=1
            continue
        fi
        extract <"$_f" | filter >"$WORK/got"
        if cmp -s "$WORK/got" "$_exp"; then
            echo "ok $_case"
        else
            echo "FAILED $_case: the extracted list differs from $_case.expected"
            diff "$_exp" "$WORK/got" | head -5 >&2
            _status=1
        fi
        _n=$((_n + 1))
    done
    rm -rf "$WORK"
    WORK=""
    if [ "$_n" -eq 0 ]; then
        echo "no captures for $_sh"
    fi
    return "$_status"
}

# ---- main ---------------------------------------------------------------------------------------------------------

[ $# -ge 1 ] || usage
SHELL_NAME=$1
case "$SHELL_NAME" in
    zsh | bash | fish | sh | dash | ksh) ;;
    *) usage ;;
esac
MODE=${2:---synthetic}

case "$MODE" in
    --synthetic) run_case "$SHELL_NAME" clean 1 ;;
    --case)
        [ $# -ge 3 ] || usage
        run_case "$SHELL_NAME" "$3" 1
        ;;
    --try)
        [ $# -ge 3 ] || usage
        run_case "$SHELL_NAME" "$3" 0
        ;;
    --real) run_real "$SHELL_NAME" ;;
    --check) run_check "$SHELL_NAME" ;;
    *) usage ;;
esac
