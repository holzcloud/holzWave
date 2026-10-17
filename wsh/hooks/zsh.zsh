[[ -n "$HOLZWAVE_IPC" ]] || return 0
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
