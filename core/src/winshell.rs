//! Windows shells: how PowerShell announces its working directory, and how the core reads the announcement.
//!
//! A pane learns the directory of its shell from the process on macOS and Linux ([`process_cwd`]). Windows has no
//! such lookup, and PowerShell does not even change the directory of its own process when it changes location, so
//! the shell has to say where it is: with OSC 7, which the engine already parses and which PowerShell never sends
//! by itself. This module is the two halves of that:
//!
//! - [`POWERSHELL_PROMPT_SCRIPT`] wraps the `prompt` function so that every prompt first writes the OSC 7 report,
//!   and [`powershell_integration_args`] is how a PowerShell pane starts it (`-NoExit -EncodedCommand ...`, which
//!   runs after the profile has loaded). [`platform::shell_launch`](crate::platform::shell_launch) is the only
//!   caller.
//! - [`windows_path_from_osc7`] and the thin [`announced_path`] read the report back, refusing everything that is
//!   not a plain drive path of this machine.
//!
//! cmd.exe has no integration: a `PROMPT` string cannot percent-encode a path, and the engine parses only OSC 7. A
//! split from a cmd.exe pane starts in the home directory. This is a documented deviation, not a bug.
//!
//! std only and compiled on every platform, so the whole module is unit-tested on a Mac
//! (`core/tests/winshell.rs`); the real PowerShell runs in `core/tests/conpty.rs` and the owner's test-plan row W7.
//!
//! [`process_cwd`]: crate::pty::process_cwd

use std::path::{Path, PathBuf};

use crate::engine::percent_decode;
use crate::platform::Os;

/// The prompt wrapper that PowerShell runs once, after its profile: it keeps the `prompt` function that is
/// current then (the user's own, a prompt framework's or the built-in one) and defines a global `prompt` that
///
/// 1. remembers whether the last command succeeded (`$?`, the first statement) and `$LASTEXITCODE`,
/// 2. when a command ran since the last prompt, writes the mark `D` (`ESC ] 133 ; D ; 0 ESC \` when it succeeded,
///    `1` otherwise), then always writes the mark `A` (a prompt starts),
/// 3. when the current location belongs to the FileSystem provider and has a drive letter, writes
///    `ESC ] 7 ; file://<COMPUTERNAME>/<drive>:/<segments> ESC \` to the console, where PowerShell itself
///    percent-encodes the machine name and every segment with `[uri]::EscapeDataString` and the drive keeps its
///    letter (a network location, a registry or a certificate drive announces nothing),
/// 4. reports a command that is new in the history to `wsh` (see below),
/// 5. puts `$LASTEXITCODE` back, and
/// 6. returns the text of the original prompt (a default `PS <path>> ` when there was none).
///
/// The history report is automatic in holzWave's own PowerShell panes, because holzWave starts this script itself; the
/// other shells need a line in their rc file. At each prompt the wrapper reads the last entry of `Get-History`; when its
/// `Id` is not the one it saw last, it starts `wsh hist add --b64 <status> <directory> <command>` without a window and
/// does not wait for it. The status is `0` when the last command succeeded and `1` otherwise (the success flag read in the
/// first statement, not an exit code). The directory and the command go in as base64 of their UTF-8 text and nothing else:
/// Windows PowerShell 5.1 and PowerShell 7 before 7.3 pass a double quote inside an argument to a native program
/// differently, so a quoted command, or a directory that ends in a backslash, would arrive cut or split. Entries that
/// exist before the first prompt are not reported. Nothing is reported for a line that starts with a space, holds a line
/// break or is longer than 3000 characters, or while the current location is not on the file system provider, or when no
/// `wsh` is on the path, or when the pane has no `HOLZWAVE_IPC` and `HOLZWAVE_TOKEN`. `wsh` and the store apply their own
/// filter on top (secrets, control characters).
///
/// The marks `B` and `C` (the command starts, its output follows) are written by an override of
/// `PSConsoleHostReadLine` that calls the original, writes both marks, remembers that a command ran and returns the
/// line unchanged. It exists only when PSReadLine (or anything else) already defines that function; without it the
/// pane writes `A` and nothing else, and the prompt text is never read or changed either way.
///
/// It is data: Rust interpolates nothing into it, so nothing a user or a program controls can reach PowerShell
/// through the argument (T-2.2-16), and a mark holds only a letter and `0` or `1`. It is valid in Windows PowerShell
/// 5.1 and in PowerShell 7, and it carries no URL with a network scheme.
///
/// Known limitation: `$?` is not preserved for the original prompt, because the wrapper's own commands reset it.
/// A prompt framework that reads `$?` may show a wrong status. The mark `D` knows only success or failure, not the
/// exit code.
pub const POWERSHELL_PROMPT_SCRIPT: &str = r##"$global:HolzwaveWsh = (Get-Command wsh -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1).Source
$global:HolzwaveLastHistoryId = (Get-History -Count 1 -ErrorAction SilentlyContinue).Id
$global:HolzwaveOriginalPrompt = $function:prompt
$global:HolzwaveOriginalReadLine = $function:PSConsoleHostReadLine
if ($global:HolzwaveOriginalReadLine) {
    function global:PSConsoleHostReadLine {
        $holzwaveLine = & $global:HolzwaveOriginalReadLine
        try {
            $esc = [string][char]27
            [Console]::Write($esc + ']133;B' + $esc + '\' + $esc + ']133;C' + $esc + '\')
            $global:HolzwaveRan = $true
        } catch {}
        $holzwaveLine
    }
}
function global:prompt {
    $holzwaveOk = $?
    $holzwaveExit = $global:LASTEXITCODE
    try {
        $esc = [string][char]27
        if ($global:HolzwaveRan) {
            $global:HolzwaveRan = $false
            [Console]::Write($esc + ']133;D;' + [int](-not $holzwaveOk) + $esc + '\')
        }
        [Console]::Write($esc + ']133;A' + $esc + '\')
        $location = $PWD
        $path = $location.ProviderPath
        if ($location.Provider.Name -eq 'FileSystem' -and $path -match '^[A-Za-z]:') {
            $names = $path.Substring(2).Split([char[]]'\/') | Where-Object { $_ -ne '' } | ForEach-Object { [uri]::EscapeDataString($_) }
            [Console]::Write($esc + ']7;file://' + [uri]::EscapeDataString("$env:COMPUTERNAME") + '/' + $path.Substring(0, 2) + '/' + ($names -join '/') + $esc + '\')
        }
    } catch {}
    try {
        $hwe = Get-History -Count 1
        if ($hwe -and $hwe.Id -ne $global:HolzwaveLastHistoryId) {
            $global:HolzwaveLastHistoryId = $hwe.Id
            $hwc = $hwe.CommandLine
            if ($global:HolzwaveWsh -and $env:HOLZWAVE_IPC -and $env:HOLZWAVE_TOKEN -and $PWD.Provider.Name -eq 'FileSystem' -and $hwc -and $hwc.Length -le 3000 -and -not $hwc.StartsWith(' ') -and $hwc -notmatch '[\r\n]') {
                $hwu = [System.Text.Encoding]::UTF8
                $hwp = New-Object System.Diagnostics.ProcessStartInfo
                $hwp.FileName = $global:HolzwaveWsh
                $hwp.Arguments = 'hist add --b64 ' + [int](-not $holzwaveOk) + ' ' + [Convert]::ToBase64String($hwu.GetBytes($PWD.ProviderPath)) + ' ' + [Convert]::ToBase64String($hwu.GetBytes($hwc))
                $hwp.UseShellExecute = $false
                $hwp.CreateNoWindow = $true
                [void][System.Diagnostics.Process]::Start($hwp)
            }
        }
    } catch {}
    $global:LASTEXITCODE = $holzwaveExit
    if ($global:HolzwaveOriginalPrompt) { & $global:HolzwaveOriginalPrompt } else { "PS $($executionContext.SessionState.Path.CurrentLocation)$('>' * ($nestedPromptLevel + 1)) " }
}
"##;

/// RFC 4648 base64 with the standard alphabet and `=` padding.
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            // A chunk of n bytes has n + 1 symbols; the rest is padding.
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((bits >> (18 - 6 * i)) & 0x3f) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// What PowerShell's `-EncodedCommand` takes: the base64 of the UTF-16LE bytes of `script`.
pub fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64_encode(&bytes)
}

/// The arguments that start the prompt wrapper in a PowerShell pane: `-NoExit`, `-EncodedCommand` and the encoded
/// [`POWERSHELL_PROMPT_SCRIPT`], in that order. They depend on nothing but the constant.
pub fn powershell_integration_args() -> Vec<String> {
    vec!["-NoExit".into(), "-EncodedCommand".into(), encode_command(POWERSHELL_PROMPT_SCRIPT)]
}

/// The Windows directory an OSC 7 report names, or `None` when it is anything but a plain drive path of this
/// machine (T-2.2-15).
///
/// `host` is the host part of the `file://` URL as written, `url_path` its path already percent-decoded (what
/// [`parse_osc7`](crate::engine::parse_osc7) returns) and `local_host` the machine's own name. A directory that this
/// result names is checked and started by the next split, and a path that names a network share would make
/// Windows authenticate to that server, so the rule is strict:
///
/// - the host is empty, `localhost` or `local_host`, compared without case after decoding it; any other host,
///   or one that does not decode, is refused;
/// - the path is `/`, then a drive (one ASCII letter and a colon), then the directories: `//server/share`, a
///   path without a drive, `/?/C:/` and `/./C:/x` are refused because their first segment is not a drive;
/// - a backslash, a control character, a `.` or `..` segment and any of `<`, `>`, `:` (after the drive), `"`, `|`,
///   `?` and `*` in a name are refused: no Windows name may hold them, and the first three are how a path walks or
///   switches to a device;
/// - empty segments (a doubled or a trailing slash) are dropped.
///
/// The result has backslashes and no trailing one, except for a drive root (`C:\`).
pub fn windows_path_from_osc7(host: &str, url_path: &str, local_host: &str) -> Option<String> {
    let host = percent_decode(host)?;
    let own = host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || (!local_host.is_empty() && host.to_lowercase() == local_host.to_lowercase());
    if !own || host.chars().any(char::is_control) {
        return None;
    }
    if url_path.chars().any(|c| c == '\\' || c.is_control()) {
        return None;
    }
    let mut segments = url_path.strip_prefix('/')?.split('/');
    let drive = segments.next()?;
    let mut letters = drive.chars();
    let is_drive = match (letters.next(), letters.next(), letters.next()) {
        (Some(letter), Some(':'), None) => letter.is_ascii_alphabetic(),
        _ => false,
    };
    if !is_drive {
        return None;
    }
    let mut path = format!("{drive}\\");
    for segment in segments.filter(|segment| !segment.is_empty()) {
        if segment == "." || segment == ".." || segment.contains(['<', '>', ':', '"', '|', '?', '*']) {
            return None;
        }
        if !path.ends_with('\\') {
            path.push('\\');
        }
        path.push_str(segment);
    }
    Some(path)
}

/// The directory a shell announced with OSC 7, as the pane uses it: on Windows the result of
/// [`windows_path_from_osc7`], on the other platforms the path as the parser returned it (a Unix path has no host or
/// drive to check, and the existence check of the next split is the same as before).
///
/// `host` and `path` are what [`parse_osc7`](crate::engine::parse_osc7) returned, `local_host` is the machine's own
/// name (`COMPUTERNAME` on Windows; unused elsewhere).
pub fn announced_path(os: Os, host: &str, path: &Path, local_host: &str) -> Option<PathBuf> {
    if os == Os::Windows {
        windows_path_from_osc7(host, path.to_str()?, local_host).map(PathBuf::from)
    } else {
        Some(path.to_path_buf())
    }
}
