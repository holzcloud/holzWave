//! Put the bundled `wsh` on a Linux host reached over SSH, by one explicit act of the person.
//!
//! The steps, in this order: ask the host what it is (`uname -sm`), pick the bundled build for its processor, ask whether the
//! `wsh` that is there already says this version, and only when it does not, upload the bundled file to
//! `<home>/.holzwave/bin/wsh` over the host's own pooled SFTP (the folders mode 0700, the file mode 0755, written aside and
//! swapped in without an old file ever being removed first) and run `wsh version` there to verify it. Nothing here runs by
//! itself: the caller is a command the person chose for the pane they have in focus. Nothing but that one file and its two
//! folders is created, no startup file of the account is read or changed (the PATH entry is the tunnel's wrapped shell's),
//! and the file is the one the release scripts bundled, never downloaded and never built on the host.
//!
//! Every `Err` is one plain sentence made of constants and the processor or system word of the host cleaned to letters,
//! digits, dot, dash and underscore: no path of this computer, no command line, no text the host sent, no credential.

use std::sync::Arc;
use std::time::Duration;

use crate::files::{FileSource, SftpSource};
use crate::pool::Connection;
use crate::ssh::{ExecCancel, ExecEnd, ExecLimits, ExecResult, SshExec};

/// The static Linux builds the app carries, read by the shell from its resource folder. `None`: this build of the app does
/// not carry that one (a build made without the release overlay).
#[derive(Clone, Default)]
pub struct Bundled {
    pub x86_64: Option<Vec<u8>>,
    pub aarch64: Option<Vec<u8>>,
}

impl std::fmt::Debug for Bundled {
    // The bytes are a program: only their sizes are shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bundled").field("x86_64", &self.x86_64.as_ref().map(Vec::len)).field("aarch64", &self.aarch64.as_ref().map(Vec::len)).finish()
    }
}

/// What an install did. Both carry the path of `wsh` on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    /// `wsh version` on the host already said the version of the app: nothing was uploaded.
    AlreadyCurrent(String),
    /// The bundled build was uploaded and `wsh version` there said the version of the app.
    Installed(String),
}

/// The folders below the home directory and the name of the file: the one place `wsh` goes.
pub const INSTALL_DIR: [&str; 2] = [".holzwave", "bin"];
pub const INSTALL_NAME: &str = "wsh";

/// The check run on the host: the same fixed text every time, built from no data. `$HOME` is the host's shell's.
const CHECK: &str = "\"$HOME/.holzwave/bin/wsh\" version 2>/dev/null";
/// The question about the system: the same fixed text every time.
const UNAME: &str = "uname -sm";

/// The sentences. Constants, so a test can pin every one of them.
pub const NO_ANSWER: &str = "wsh was not installed: the host could not be asked what system it runs";
pub const NO_SYSTEM: &str = "wsh was not installed: this host did not say what system it runs";
pub const NO_SFTP: &str = "wsh was not installed: file transfer (SFTP) is not available on this host";
pub const REFUSED: &str = "wsh was not installed: this host refused the upload";
pub const NOT_VERIFIED: &str = "wsh was copied to this host but could not be verified: running it there did not print its version";
pub const NOT_ELF: &str = "wsh was not installed: the wsh in this build of holzWave is not a Linux program";

fn not_linux(system: &str) -> String {
    format!("wsh was not installed: this host runs {system}, and wsh is installed on Linux hosts only")
}

fn no_architecture(arch: &str) -> String {
    format!("wsh was not installed: there is no wsh for the processor type {arch} (only x86_64 and aarch64 are supported)")
}

fn no_binary(arch: &str) -> String {
    format!("wsh was not installed: this build of holzWave carries no wsh for {arch}")
}

/// The most characters of a word of the host that a sentence repeats.
const WORD_LIMIT: usize = 24;

/// A word the host said, made safe to show: letters, digits, dot, dash and underscore only, and short.
fn clean_word(word: &str) -> String {
    word.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).take(WORD_LIMIT).collect()
}

/// The last non-empty line of the output of a command, trimmed: a login shell may print a banner before the answer.
fn last_line(result: &ExecResult) -> Option<String> {
    let text = String::from_utf8_lossy(&result.stdout);
    text.lines().map(str::trim).rfind(|line| !line.is_empty()).map(str::to_owned)
}

/// The two words of `uname -sm` the build is picked by.
#[derive(Debug)]
enum Arch {
    X86_64,
    Aarch64,
}

/// Parse the answer of `uname -sm` into the processor, or the sentence that says why this host cannot take `wsh`.
fn detect(line: Option<&str>) -> Result<Arch, String> {
    let words: Vec<&str> = line.map(|l| l.split_whitespace().collect()).unwrap_or_default();
    let [system, arch] = words[..] else { return Err(NO_SYSTEM.into()) };
    if system != "Linux" {
        return Err(not_linux(&clean_word(system)));
    }
    match arch {
        "x86_64" | "amd64" => Ok(Arch::X86_64),
        "aarch64" | "arm64" => Ok(Arch::Aarch64),
        other => Err(no_architecture(&clean_word(other))),
    }
}

/// Run one fixed command on the connection with small bounds.
fn ask(exec: &SshExec, command: &str) -> Option<ExecResult> {
    let limits = ExecLimits { max_output: 4096, timeout: Duration::from_secs(10) };
    exec.exec(command, &limits, &ExecCancel::new()).ok()
}

/// Whether the `wsh` on the host says exactly the version of the app.
fn says_version(exec: &SshExec, app_version: &str) -> bool {
    ask(exec, CHECK)
        .filter(|r| r.end == ExecEnd::Finished && r.status == Some(0))
        .and_then(|r| last_line(&r))
        .is_some_and(|line| line == format!("wsh {app_version}"))
}

/// Install `wsh` on the host of `connection` as the module says. Blocks (a login at most, a few round trips and the upload):
/// never call it from the UI thread.
pub fn install(connection: &Arc<Connection>, bundled: &Bundled, app_version: &str) -> Result<Installed, String> {
    // The connection counts the whole install as one use, so the idle reaper never closes it between the steps.
    let _lease = connection.lease();
    let exec = SshExec::on(connection.clone()).map_err(|_| NO_ANSWER.to_owned())?;

    let answer = ask(&exec, UNAME).filter(|r| r.end == ExecEnd::Finished && r.status == Some(0)).ok_or_else(|| NO_ANSWER.to_owned())?;
    let arch = detect(last_line(&answer).as_deref())?;
    let (bytes, name) = match arch {
        Arch::X86_64 => (&bundled.x86_64, "x86_64"),
        Arch::Aarch64 => (&bundled.aarch64, "aarch64"),
    };
    let Some(bytes) = bytes else { return Err(no_binary(name)) };
    // The upload is a program that will run in the person's account: it must at least be one for Linux.
    if !bytes.starts_with(b"\x7fELF") {
        return Err(NOT_ELF.into());
    }

    let files = SftpSource::on(connection.clone());
    let home = files.home().map_err(|_| NO_SFTP.to_owned())?;
    let path = format!("{}/{}/{}/{}", home.trim_end_matches('/'), INSTALL_DIR[0], INSTALL_DIR[1], INSTALL_NAME);
    if says_version(&exec, app_version) {
        return Ok(Installed::AlreadyCurrent(path));
    }
    let placed = files.install_executable(&INSTALL_DIR, INSTALL_NAME, bytes).map_err(|_| REFUSED.to_owned())?;
    if !says_version(&exec, app_version) {
        return Err(NOT_VERIFIED.into());
    }
    Ok(Installed::Installed(placed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_is_read_from_two_words() {
        assert!(matches!(detect(Some("Linux x86_64")), Ok(Arch::X86_64)));
        assert!(matches!(detect(Some("Linux amd64")), Ok(Arch::X86_64)));
        assert!(matches!(detect(Some("Linux aarch64")), Ok(Arch::Aarch64)));
        assert!(matches!(detect(Some("Linux arm64")), Ok(Arch::Aarch64)));
        assert!(detect(Some("Darwin arm64")).unwrap_err().contains("runs Darwin"));
        assert!(detect(Some("Linux armv7l")).unwrap_err().contains("armv7l"));
        for garbled in [None, Some(""), Some("Linux"), Some("Linux x86_64 extra")] {
            assert_eq!(detect(garbled).unwrap_err(), NO_SYSTEM);
        }
    }

    #[test]
    fn a_word_of_the_host_is_cleaned_and_short() {
        assert_eq!(clean_word("Lin\u{1b}[31mux"), "Lin31mux");
        assert_eq!(clean_word("a/b c;d$(x)"), "abcdx");
        assert_eq!(clean_word(&"x".repeat(100)).len(), WORD_LIMIT);
    }
}
