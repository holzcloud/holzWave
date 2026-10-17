if [[ -n "${HOLZWAVE_IPC-}" && -z "${__holzwave_hook-}" ]]; then
  typeset -g __holzwave_hook=1
  typeset -g __holzwave_ran= __holzwave_cmd=
  __holzwave_preexec() {
    __holzwave_cmd=$1
    __holzwave_ran=1
    builtin printf '\033]133;B\033\\\033]133;C\033\\'
  }
  __holzwave_precmd() {
    local s=$?
    if [[ -n "${__holzwave_ran-}" ]]; then
      __holzwave_ran=
      builtin printf '\033]133;D;%d\033\\' "$s"
    fi
    builtin printf '\033]133;A\033\\'
    if [[ -n "${__holzwave_cmd-}" ]]; then
      (wsh hist add "$s" "$PWD" "$__holzwave_cmd" >/dev/null 2>&1 &)
      __holzwave_cmd=
    fi
    return $s
  }
  autoload -Uz add-zsh-hook
  add-zsh-hook preexec __holzwave_preexec
  add-zsh-hook precmd __holzwave_precmd
fi
