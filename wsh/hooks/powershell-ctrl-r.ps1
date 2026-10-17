# Ctrl+R opens the holzWave history dialog for this pane. Added by `wsh init powershell --ctrl-r`; the profile loads its
# output (see docs/config.md). It needs PSReadLine and a new shell, or a second load, to take effect.
# Only Ctrl+R is bound (the Windows and Emacs modes, and Vi insert mode); the typed line is left alone, and wsh starts
# without a window. Another tool that binds Ctrl+R (a fuzzy finder, a history tool) wins or loses by the order of the lines
# in the profile: the later one wins.
if ($env:HOLZWAVE_IPC -and (Get-Command Set-PSReadLineKeyHandler -ErrorAction SilentlyContinue)) {
  $holzwaveOpenHistory = {
    try {
      $wsh = Get-Command wsh -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
      if ($wsh) {
        $info = New-Object System.Diagnostics.ProcessStartInfo
        $info.FileName = $wsh.Source
        $info.Arguments = 'hist open'
        $info.UseShellExecute = $false
        $info.CreateNoWindow = $true
        [void][System.Diagnostics.Process]::Start($info)
      }
    } catch {}
  }
  try { Set-PSReadLineKeyHandler -Chord 'Ctrl+r' -BriefDescription 'HolzwaveHistory' -ScriptBlock $holzwaveOpenHistory -ErrorAction SilentlyContinue -WarningAction SilentlyContinue } catch {}
  try { Set-PSReadLineKeyHandler -ViMode Insert -Chord 'Ctrl+r' -BriefDescription 'HolzwaveHistory' -ScriptBlock $holzwaveOpenHistory -ErrorAction SilentlyContinue -WarningAction SilentlyContinue } catch {}
}
