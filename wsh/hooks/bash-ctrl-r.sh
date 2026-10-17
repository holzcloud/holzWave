# Ctrl+R opens the holzWave history dialog for this pane. Added by `wsh init bash --ctrl-r`, after the history hook.
# Only Ctrl+R is bound (emacs and vi insert mode); the typed line, the prompt and the exit status are left alone.
# Needs a new shell, or loading this text a second time, to take effect. Another tool that binds Ctrl+R (a fuzzy finder,
# a history tool) wins or loses by the order of the lines in the rc file: the later one wins.
if [ -n "${HOLZWAVE_IPC-}" ] && [ -z "${__holzwave_ctrl_r-}" ] && [ "${-#*i}" != "$-" ] && [ "${BASH_VERSINFO[0]:-0}" -ge 4 ]; then
  __holzwave_ctrl_r=1
  __holzwave_open_history() {
    (wsh hist open >/dev/null 2>&1 &)
    return 0
  }
  bind -m emacs -x '"\C-r": __holzwave_open_history' 2>/dev/null
  bind -m vi-insert -x '"\C-r": __holzwave_open_history' 2>/dev/null
fi
