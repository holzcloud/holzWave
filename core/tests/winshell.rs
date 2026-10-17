//! Tests for the Windows shell integration: the PowerShell prompt wrapper and its `-EncodedCommand` argument, and the
//! Windows reading of an OSC 7 path (T-2.2-15, T-2.2-16).
//!
//! Nothing here starts PowerShell: the module is plain string work and runs on every host. The real run is
//! `core/tests/conpty.rs` and the owner's row W7.

use std::path::{Path, PathBuf};

use holzwave_core::engine::parse_osc7;
use holzwave_core::platform::Os;
use holzwave_core::winshell::{
    POWERSHELL_PROMPT_SCRIPT, announced_path, base64_encode, encode_command, powershell_integration_args,
    windows_path_from_osc7,
};

// ---- the encoder -------------------------------------------------------------------------------------

#[test]
fn rfc_4648_vectors() {
    // Section 10 of RFC 4648.
    for (plain, encoded) in [
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ] {
        assert_eq!(base64_encode(plain.as_bytes()), encoded, "{plain:?}");
    }
    // All 64 symbols, and the two that are not letters or digits.
    assert_eq!(base64_encode(&[0xfb, 0xff, 0xbf]), "+/+/");
}

/// A decoder written for the test, independent of the encoder under test.
fn base64_decode(text: &str) -> Vec<u8> {
    let symbol = |c: char| "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".find(c).unwrap() as u32;
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    assert_eq!(chars.len() % 4, 0, "base64 is padded to a multiple of four");
    for quad in chars.chunks(4) {
        let pad = quad.iter().filter(|c| **c == '=').count();
        let bits = quad.iter().fold(0u32, |acc, c| acc << 6 | if *c == '=' { 0 } else { symbol(*c) });
        let bytes = [(bits >> 16) as u8, (bits >> 8) as u8, bits as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    out
}

fn utf16le_to_string(bytes: &[u8]) -> String {
    assert_eq!(bytes.len() % 2, 0, "UTF-16 has an even number of bytes");
    let units: Vec<u16> = bytes.chunks(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
    String::from_utf16(&units).expect("valid UTF-16")
}

#[test]
fn the_encoded_command_is_utf16le_with_padding() {
    // 'A' is the bytes 41 00.
    assert_eq!(encode_command("A"), "QQA=");
    // A character outside the Basic Multilingual Plane is a surrogate pair, two units of two bytes each.
    assert_eq!(base64_decode(&encode_command("\u{1F600}")), [0x3d, 0xd8, 0x00, 0xde]);
    assert_eq!(encode_command(""), "");
}

#[test]
fn the_encoded_command_decodes_to_the_script_in_utf16le() {
    let args = powershell_integration_args();
    let encoded = &args[2];
    assert_eq!(utf16le_to_string(&base64_decode(encoded)), POWERSHELL_PROMPT_SCRIPT);
    assert_eq!(*encoded, encode_command(POWERSHELL_PROMPT_SCRIPT));
    assert!(encoded.len() % 4 == 0 && encoded.chars().all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c)));
}

#[test]
fn the_integration_arguments_are_exactly_no_exit_encoded_command_and_the_script() {
    let args = powershell_integration_args();
    assert_eq!(args.len(), 3);
    assert_eq!(args[0], "-NoExit");
    assert_eq!(args[1], "-EncodedCommand");
    // Nothing varies: two calls give the same arguments.
    assert_eq!(args, powershell_integration_args());
}

// ---- the script --------------------------------------------------------------------------------------

#[test]
fn the_script_is_plain_ascii_without_a_network_scheme() {
    assert!(POWERSHELL_PROMPT_SCRIPT.is_ascii());
    for scheme in ["http:", "https:", "ftp:", "ws:", "wss:", "\\\\"] {
        assert!(!POWERSHELL_PROMPT_SCRIPT.contains(scheme), "the script contains {scheme:?}");
    }
    // Every `://` is the `file` scheme.
    for (at, _) in POWERSHELL_PROMPT_SCRIPT.match_indices("://") {
        assert!(POWERSHELL_PROMPT_SCRIPT[..at].ends_with("file"), "a scheme other than file at {at}");
    }
}

#[test]
fn the_script_wraps_the_prompt_and_reads_and_encodes_everything_itself() {
    let script = POWERSHELL_PROMPT_SCRIPT;
    // The original prompt is kept and called; the new one is global.
    assert!(script.contains("$function:prompt"));
    assert!(script.contains("function global:prompt"));
    // The machine name and the path are read and percent-encoded by PowerShell.
    assert!(script.contains("$env:COMPUTERNAME"));
    assert!(script.contains("[uri]::EscapeDataString"));
    assert!(script.contains("ProviderPath"));
    // OSC 7 with the string terminator ESC \ and only for the FileSystem provider.
    assert!(script.contains("]7;file://"));
    assert!(script.contains("FileSystem"));
    // $LASTEXITCODE is read before and put back after the wrapper's own work.
    let first = script.find("$global:LASTEXITCODE").expect("reads $LASTEXITCODE");
    let last = script.rfind("$global:LASTEXITCODE").expect("restores $LASTEXITCODE");
    assert!(first < last, "restored after it was read");
    assert!(script[last..].contains('=') && script[..last].contains("HolzwaveOriginalPrompt"));
}

/// The body of the global `prompt` function.
fn prompt_body() -> &'static str {
    let script = POWERSHELL_PROMPT_SCRIPT;
    &script[script.find("function global:prompt").expect("the prompt function")..]
}

#[test]
fn the_script_marks_a_command_at_its_four_points() {
    let script = POWERSHELL_PROMPT_SCRIPT;
    // The mark ends with the string terminator, ESC and a single backslash; the escape is built from its number.
    assert!(script.contains("$esc = [string][char]27"));
    for mark in ["]133;A' + $esc + '\\')", "]133;B' + $esc + '\\'", "]133;C' + $esc + '\\')", "]133;D;' + [int](-not $holzwaveOk) + $esc + '\\')"] {
        assert!(script.contains(mark), "{mark}");
    }
    // A mark holds a letter and, for D, one digit computed from the status: nothing else is written into it.
    assert!(!script.contains("133;D;' + $"));
    // In the prompt: D before A before the OSC 7 report, and D only once a command ran.
    let body = prompt_body();
    let d = body.find("]133;D;").expect("D");
    let a = body.find("]133;A").expect("A");
    let osc7 = body.find("]7;file://").expect("OSC 7");
    assert!(d < a && a < osc7, "D, then A, then the directory");
    assert!(body[..d].contains("if ($global:HolzwaveRan)") && body[..d].contains("$global:HolzwaveRan = $false"));
    // B and C come from the read-line override, together, before the flag is set.
    let over = &script[script.find("function global:PSConsoleHostReadLine").expect("override")..script.find("function global:prompt").unwrap()];
    assert!(over.find("]133;B").unwrap() < over.find("]133;C").unwrap());
    assert!(over.find("]133;C").unwrap() < over.find("$global:HolzwaveRan = $true").unwrap());
}

#[test]
fn the_first_statement_of_the_prompt_reads_the_status() {
    let body = prompt_body();
    let open = body.find('{').expect("the body opens");
    let first = body[open + 1..].lines().map(str::trim).find(|l| !l.is_empty()).expect("a statement");
    assert_eq!(first, "$holzwaveOk = $?");
    // The status of D comes from `$?`, never from $LASTEXITCODE, so the pinned order of its two accesses stays.
    let marks = &body[body.find("]133;D;").unwrap()..body.find("]133;A").unwrap()];
    assert!(!marks.contains("LASTEXITCODE"));
}

#[test]
fn the_read_line_override_exists_only_when_the_original_does() {
    let script = POWERSHELL_PROMPT_SCRIPT;
    let save = script.find("$global:HolzwaveOriginalReadLine = $function:PSConsoleHostReadLine").expect("saves the original");
    let guard = script.find("if ($global:HolzwaveOriginalReadLine) {").expect("the guard");
    let over = script.find("function global:PSConsoleHostReadLine").expect("the override");
    assert!(save < guard && guard < over, "the override sits inside the guard");
    let end = script.find("function global:prompt").unwrap();
    let block = &script[guard..end];
    assert!(block.contains("& $global:HolzwaveOriginalReadLine"), "the original is called");
    assert!(block.trim_end().ends_with('}'), "the guard closes before the prompt");
    // The line is returned unchanged, and the writes sit in a try so a console error never loses it.
    assert!(block.contains("$holzwaveLine\n    }"));
    assert!(block.find("try {").unwrap() < block.find("[Console]::Write").unwrap());
    assert!(block.find("} catch {}").unwrap() < block.find("$holzwaveLine\n    }").unwrap());
}

#[test]
fn the_encoded_argument_stays_far_below_the_command_line_limit() {
    // The limit of a Windows command line is 32767 characters; the argument has to stay well below it.
    assert!(encode_command(POWERSHELL_PROMPT_SCRIPT).len() < 16000);
    assert!(powershell_integration_args().iter().map(String::len).sum::<usize>() < 16000);
}

// ---- the Windows reading of an OSC 7 path ------------------------------------------------------------

fn read(host: &str, path: &str) -> Option<String> {
    windows_path_from_osc7(host, path, "host")
}

#[test]
fn a_drive_path_from_the_own_host_becomes_a_windows_path() {
    assert_eq!(read("HOST", "/C:/Users/me/My Docs").as_deref(), Some(r"C:\Users\me\My Docs"));
    assert_eq!(read("host", "/D:/a/b").as_deref(), Some(r"D:\a\b"));
    assert_eq!(read("HoSt", "/C:/x").as_deref(), Some(r"C:\x"));
}

#[test]
fn an_empty_host_and_localhost_are_accepted() {
    assert_eq!(read("", "/C:/Users").as_deref(), Some(r"C:\Users"));
    assert_eq!(read("localhost", "/C:/Users").as_deref(), Some(r"C:\Users"));
    assert_eq!(read("LocalHost", "/C:/Users").as_deref(), Some(r"C:\Users"));
}

#[test]
fn a_host_name_that_is_percent_encoded_is_compared_decoded() {
    assert_eq!(windows_path_from_osc7("J%C3%9CRGEN-PC", "/C:/x", "j\u{fc}rgen-pc").as_deref(), Some(r"C:\x"));
}

#[test]
fn drive_roots_give_the_root_with_a_backslash() {
    assert_eq!(read("", "/C:/").as_deref(), Some(r"C:\"));
    assert_eq!(read("", "/C:").as_deref(), Some(r"C:\"));
    assert_eq!(read("", "/c:/").as_deref(), Some(r"c:\"));
}

#[test]
fn a_trailing_slash_and_a_doubled_slash_do_not_matter() {
    assert_eq!(read("", "/C:/Users/me/").as_deref(), Some(r"C:\Users\me"));
    assert_eq!(read("", "/C://Users///me").as_deref(), Some(r"C:\Users\me"));
}

#[test]
fn a_foreign_host_is_refused() {
    assert_eq!(read("evil", "/C:/x"), None);
    assert_eq!(read("host.example.org", "/C:/x"), None);
    assert_eq!(read("host:445", "/C:/x"), None);
    assert_eq!(read("user@host", "/C:/x"), None);
    // The machine's own name is not a prefix match.
    assert_eq!(read("hostile", "/C:/x"), None);
    // Without a known own name only the empty host and localhost remain.
    assert_eq!(windows_path_from_osc7("HOST", "/C:/x", ""), None);
    assert_eq!(windows_path_from_osc7("", "/C:/x", "").as_deref(), Some(r"C:\x"));
    // An undecodable host is refused, not read as written.
    assert_eq!(read("%zz", "/C:/x"), None);
}

#[test]
fn a_unc_path_is_refused() {
    assert_eq!(read("", "//server/share/x"), None);
    assert_eq!(read("", "//server"), None);
    assert_eq!(read("", "/server/share"), None);
    assert_eq!(read("host", "//host/C:/x"), None);
    assert_eq!(read("", "//"), None);
}

#[test]
fn device_and_extended_forms_are_refused() {
    assert_eq!(read("", "/?/C:/"), None);
    assert_eq!(read("", "/./C:/x"), None);
    assert_eq!(read("", "/??/C:/x"), None);
    assert_eq!(read("", "/C:/?/x"), None);
    assert_eq!(read("", "/GLOBALROOT/Device/x"), None);
}

#[test]
fn a_path_that_is_not_absolute_or_has_no_drive_is_refused() {
    for path in ["", "/", "C:/x", "C:", "x/C:/y", "/Users/me", "/C/x", "/CC:/x", "/1:/x", "/\u{e4}:/x", "/ :/x"] {
        assert_eq!(read("", path), None, "{path:?}");
    }
}

#[test]
fn a_backslash_in_the_decoded_text_is_refused() {
    assert_eq!(read("", r"/C:/a\b"), None);
    assert_eq!(read("", r"/C:\x"), None);
    assert_eq!(read("", r"/\\?\C:/x"), None);
}

#[test]
fn a_dot_dot_segment_is_refused() {
    assert_eq!(read("", "/C:/a/../b"), None);
    assert_eq!(read("", "/C:/.."), None);
    assert_eq!(read("", "/C:/a/./b"), None);
    // A name that merely contains dots is a name.
    assert_eq!(read("", "/C:/a..b/..c/d.").as_deref(), Some(r"C:\a..b\..c\d."));
}

#[test]
fn a_character_no_windows_name_may_hold_is_refused() {
    for bad in ["<", ">", ":", "\"", "|", "?", "*"] {
        assert_eq!(read("", &format!("/C:/a{bad}b")), None, "{bad:?} inside a name");
        assert_eq!(read("", &format!("/C:/x/{bad}")), None, "{bad:?} as a name");
    }
    // `:` is the drive's and nothing else's; `&`, `%`, `'`, `;`, `[` and `$` are fine in a name.
    assert_eq!(read("", "/C:/a&b/c%d/'e'/f;g/[h]/$i").as_deref(), Some(r"C:\a&b\c%d\'e'\f;g\[h]\$i"));
}

#[test]
fn a_control_character_is_refused() {
    for bad in ["\u{0}", "\u{1b}", "\n", "\r", "\t", "\u{7f}", "\u{85}"] {
        assert_eq!(read("", &format!("/C:/a{bad}b")), None, "{bad:?}");
        assert_eq!(windows_path_from_osc7(&format!("ho{bad}st"), "/C:/x", "host"), None, "{bad:?} in the host");
    }
}

#[test]
fn non_ascii_names_pass() {
    assert_eq!(read("", "/C:/Users/J\u{fc}rgen M\u{fc}ller").as_deref(), Some("C:\\Users\\J\u{fc}rgen M\u{fc}ller"));
}

// ---- announced_path ----------------------------------------------------------------------------------

#[test]
fn on_unix_the_announced_path_is_returned_unchanged() {
    for os in [Os::Macos, Os::Linux] {
        for (host, path) in [("anything", "/Users/me/x"), ("", "/srv/a b"), ("h", "//double/slash"), ("h", "relative")] {
            let got = announced_path(os, host, Path::new(path), "me-mac");
            assert_eq!(got, Some(PathBuf::from(path)), "{os:?} {path}");
        }
    }
}

#[test]
fn on_windows_the_announced_path_goes_through_the_refusing_reader() {
    let at = |host: &str, path: &str| announced_path(Os::Windows, host, Path::new(path), "host");
    assert_eq!(at("HOST", "/C:/Users/me"), Some(PathBuf::from(r"C:\Users\me")));
    assert_eq!(at("evil", "/C:/x"), None);
    assert_eq!(at("", "//evil/share"), None);
    assert_eq!(at("", "/Users/me"), None);
}

// ---- T-2.2-15 end to end: from the OSC 7 value ------------------------------------------------------

/// What the pane does with the engine's report: the parser first, then the Windows rule.
fn from_osc7(value: &str) -> Option<PathBuf> {
    let (host, path) = parse_osc7(value)?;
    announced_path(Os::Windows, &host, &path, "host")
}

#[test]
fn a_prompt_value_from_the_own_machine_is_read() {
    assert_eq!(from_osc7("file://HOST/C:/Users/me/My%20Docs"), Some(PathBuf::from(r"C:\Users\me\My Docs")));
    assert_eq!(from_osc7("file:///C:/Users"), Some(PathBuf::from(r"C:\Users")));
    assert_eq!(from_osc7("file://localhost/D:/"), Some(PathBuf::from(r"D:\")));
}

#[test]
fn osc7_values_that_name_a_foreign_server_are_refused() {
    for value in [
        "file://evil/C:/x",
        "file://evil/share/x",
        "file:////evil/share",
        "file://///evil/share/x",
        "file://HOST//evil/share",
        "file:///%5C%5Cevil%5Cshare",
        "file:///%5C%5C%3F%5CC:/x",
        "file:///%5C%5C.%5CC:/x",
        "file:///%2F%2Fevil/share",
        "file:///%3F/C:/x",
        "file:///C:/..%5Cx",
        "file:///C:/a%5Cb",
        "file:///C:/a%2F..%2Fb",
        "file://HOST/C:/a%3Ab",
        "file:///C:/a%2Ab",
    ] {
        assert_eq!(from_osc7(value), None, "{value}");
    }
}

// ---- the contract between the script's formula and the parser ----------------------------------------

/// The formula of the script, on the Unix side: the machine name and each segment after the drive are percent-encoded
/// byte by byte except letters, digits and `-_.~`; the segments are joined with `/`; the drive is the first segment.
fn script_url(host: &str, path: &str) -> String {
    let encode = |text: &str| -> String {
        text.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    };
    let (drive, rest) = path.split_at(2);
    let segments: Vec<String> = rest.split('\\').filter(|s| !s.is_empty()).map(encode).collect();
    format!("file://{}/{drive}/{}", encode(host), segments.join("/"))
}

#[test]
fn the_script_formula_round_trips_through_the_parser() {
    for path in [
        "C:\\Users\\J\u{fc}rgen M\u{fc}ller\\Dev Projects",
        r"D:\a&b\c%d",
        r"C:\",
        r"E:\x'y\(z)\[w]\{v}\a;b\c,d\e=f\g+h\i@j\k#l\m!n\$o\p^q",
        "C:\\Users\\\u{65e5}\u{672c}\u{8a9e}\\\u{1F600}",
        r"C:\a.b\c-d\e_f\g~h",
    ] {
        for host in ["HOST", "DESKTOP-4F7K2", ""] {
            let url = script_url(host, path);
            let (parsed_host, url_path) = parse_osc7(&url).unwrap_or_else(|| panic!("the parser refuses {url}"));
            let local = if host.is_empty() { "HOST" } else { host };
            let got = windows_path_from_osc7(&parsed_host, &url_path.to_string_lossy(), local);
            assert_eq!(got.as_deref(), Some(path), "{url}");
        }
    }
}

#[test]
fn the_script_formula_gives_a_drive_root_for_a_root() {
    assert_eq!(script_url("HOST", r"C:\"), "file://HOST/C:/");
    let (host, path) = parse_osc7("file://HOST/C:/").unwrap();
    assert_eq!(windows_path_from_osc7(&host, &path.to_string_lossy(), "HOST").as_deref(), Some(r"C:\"));
}
