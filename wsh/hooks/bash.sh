if [ -n "${HOLZWAVE_IPC-}" ] && [ -z "${__holzwave_hook-}" ]; then
  __holzwave_hook=1
  __holzwave_ran=
  __holzwave_hist() {
    local s=$? c
    c=$(HISTTIMEFORMAT= builtin history 1 | sed 's/^ *[0-9]*[* ] *//')
    if [ -n "$c" ] && [ "$c" != "${__holzwave_last-}" ]; then
      __holzwave_last=$c
      (wsh hist add "$s" "$PWD" "$c" >/dev/null 2>&1 &)
    fi
    return $s
  }
  if [ "${BASH_VERSINFO[0]:-0}" -gt 4 ] || { [ "${BASH_VERSINFO[0]:-0}" -eq 4 ] && [ "${BASH_VERSINFO[1]:-0}" -ge 4 ]; }; then
    if shopt -q promptvars; then
      __holzwave_marks() {
        local s=$?
        if [ -n "${__holzwave_ran-}" ]; then
          __holzwave_ran=
          builtin printf '\033]133;D;%d\033\\' "$s"
        fi
        builtin printf '\033]133;A\033\\'
        return $s
      }
      PS0="${PS0-}"'${__holzwave_ran[$((__holzwave_ran=1))]-}\033]133;B\033\\\033]133;C\033\\'
      PROMPT_COMMAND="__holzwave_marks;__holzwave_hist${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
    else
      PROMPT_COMMAND="__holzwave_hist${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
    fi
  else
    PROMPT_COMMAND="__holzwave_hist${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
  fi
fi
