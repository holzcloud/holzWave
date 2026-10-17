if test -n "$HOLZWAVE_IPC"; and not set -q __holzwave_hook
  set -g __holzwave_hook 1
  if not status test-feature mark-prompt 2>/dev/null
    function __holzwave_prompt --on-event fish_prompt
      printf '\033]133;A\033\\\\'
    end
    function __holzwave_preexec --on-event fish_preexec
      set -g __holzwave_ran 1
      printf '\033]133;B\033\\\\\033]133;C\033\\\\'
    end
  end
  function __holzwave_postexec --on-event fish_postexec
    set -l s $status
    if set -q __holzwave_ran
      set -e __holzwave_ran
      printf '\033]133;D;%d\033\\\\' $s
    end
    wsh hist add $s $PWD "$argv[1]" >/dev/null 2>&1 &
  end
end
