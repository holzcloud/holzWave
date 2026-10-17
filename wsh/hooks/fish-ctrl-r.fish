# Ctrl+R opens the holzWave history dialog for this pane. Added by `wsh init fish --ctrl-r`, after the history hook.
# Only Ctrl+R is bound (default and insert modes); the typed line, the prompt and the exit status are left alone.
# Needs a new shell, or loading this text a second time, to take effect. Another tool that binds Ctrl+R (a fuzzy finder,
# a history tool) wins or loses by the order of the lines in config.fish: the later one wins.
if test -n "$HOLZWAVE_IPC"; and not set -q __holzwave_ctrl_r; and status is-interactive
  set -g __holzwave_ctrl_r 1
  function __holzwave_open_history
    set -l s $status
    command wsh hist open >/dev/null 2>&1 &
    commandline -f repaint
    return $s
  end
  bind \cr __holzwave_open_history
  bind -M insert \cr __holzwave_open_history
end
