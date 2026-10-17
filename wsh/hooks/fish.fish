test -n "$HOLZWAVE_IPC"; or exit 0
function __holzwave_postexec --on-event fish_postexec
  set -l s $status
  wsh hist add $s $PWD "$argv[1]" >/dev/null 2>&1 &
end
