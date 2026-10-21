//! The fake program rig of the integration tests: a script that stands in for a command-line tool.
//!
//! A fake records what it was started with, then behaves as chosen. The recording goes to files next to the script
//! (`<name>.argv`: the arguments as NUL-terminated bytes, `<name>.env`: the environment as NUL-terminated
//! `NAME=VALUE` entries, `<name>.ppid`, `<name>.pgid`, `<name>.child`), so a test reads back exactly what the program
//! received.
//!
//! There is one script text, a constant of this file: every fake is a hard link to it under its own name, and its
//! behaviour comes from files next to the link (`<name>.conf` holds numbers the rig formatted, `<name>.out` and
//! `<name>.err` hold the text to print). Nothing a test passes is interpolated into a script. The one script is
//! started once per test process before any fake exists: the first start of a new executable file can take 15 to 45
//! seconds on a Mac whose system policy daemon is busy, and a hard link to a file that was started once does not pay
//! it again, so a test of a time limit never measures that.
//!
//! The scripts are POSIX shell, so the rig is Unix only.
#![allow(dead_code)]
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The text of every fake. The warm mode (an environment variable only the rig sets) exits before anything is recorded.
const SCRIPT: &str = "#!/bin/sh
if [ -n \"$HW_FAKE_WARM\" ]; then exit 0; fi
d=${0%/*}
n=${0##*/}
stdin=0; out_bytes=0; err_bytes=0; sleep_secs=0; child_secs=0; orphan_secs=0; exit_code=0
if [ -f \"$d/$n.conf\" ]; then . \"$d/$n.conf\"; fi
for a in \"$@\"; do printf '%s\\0' \"$a\"; done > \"$d/$n.argv\"
/usr/bin/env -0 > \"$d/$n.env\"
echo \"$PPID\" > \"$d/$n.ppid\"
/bin/ps -o pgid= -p \"$$\" | tr -d ' ' > \"$d/$n.pgid\"
if [ \"$stdin\" = 1 ]; then cat; fi
if [ -f \"$d/$n.out\" ]; then cat \"$d/$n.out\"; fi
if [ -f \"$d/$n.err\" ]; then cat \"$d/$n.err\" >&2; fi
if [ \"$out_bytes\" -gt 0 ]; then yes 0123456789abcdef0123456789abcde | head -c \"$out_bytes\"; fi
if [ \"$err_bytes\" -gt 0 ]; then yes 0123456789abcdef0123456789abcde | head -c \"$err_bytes\" >&2; fi
if [ \"$child_secs\" -gt 0 ]; then sleep \"$child_secs\" & echo \"$!\" > \"$d/$n.child\"; wait; fi
if [ \"$orphan_secs\" -gt 0 ]; then sleep \"$orphan_secs\" & echo \"$!\" > \"$d/$n.child\"; fi
if [ \"$sleep_secs\" -gt 0 ]; then sleep \"$sleep_secs\"; fi
exit \"$exit_code\"
";

/// The one script as a file: content-addressed in the build's scratch directory, written once, started once.
fn master() -> &'static Path {
    static MASTER: OnceLock<PathBuf> = OnceLock::new();
    MASTER.get_or_init(|| {
        let hash = SCRIPT
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        // An integration test of this crate has the build's scratch directory; the unit tests of another crate that
        // include this file by path do not, and use the system's.
        let base = option_env!("CARGO_TARGET_TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let dir = base.join("fake_cli");
        fs::create_dir_all(&dir).expect("the build's scratch directory");
        let path = dir.join(format!("base-{hash:016x}"));
        if !path.exists() {
            let temporary = dir.join(format!("base-{hash:016x}.{}", std::process::id()));
            fs::write(&temporary, SCRIPT).expect("write the script");
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755)).expect("make the script executable");
            fs::rename(&temporary, &path).expect("put the script in place");
        }
        let status = std::process::Command::new(&path)
            .env("HW_FAKE_WARM", "1")
            .stdin(std::process::Stdio::null())
            .status()
            .expect("start the script once");
        assert!(status.success(), "the warm start exits at once");
        path
    })
}

/// A fake program in a scratch directory. Every builder method rewrites the settings, so a test can chain them.
#[derive(Debug, Clone)]
pub struct FakeCli {
    dir: PathBuf,
    name: String,
    reads_stdin: bool,
    out_bytes: usize,
    err_bytes: usize,
    sleep_secs: Option<u32>,
    child_secs: Option<u32>,
    orphan_secs: Option<u32>,
    exit: i32,
    out_text: Option<String>,
    err_text: Option<String>,
}

impl FakeCli {
    /// A fake named `name` in `dir`: it records its start and exits with status 0 without output.
    pub fn new(dir: &Path, name: &str) -> FakeCli {
        assert!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
        let fake = FakeCli {
            dir: dir.to_path_buf(),
            name: name.to_owned(),
            reads_stdin: false,
            out_bytes: 0,
            err_bytes: 0,
            sleep_secs: None,
            child_secs: None,
            orphan_secs: None,
            exit: 0,
            out_text: None,
            err_text: None,
        };
        let link = fake.path();
        let _ = fs::remove_file(&link);
        if fs::hard_link(master(), &link).is_err() {
            fs::copy(master(), &link).expect("put the fake in place");
        }
        fake.write();
        fake
    }

    /// The fake prints `text` on standard output (exactly these bytes).
    pub fn prints(mut self, text: &str) -> FakeCli {
        self.out_text = Some(text.to_owned());
        self.write();
        self
    }

    /// The fake prints `text` on standard error.
    pub fn prints_stderr(mut self, text: &str) -> FakeCli {
        self.err_text = Some(text.to_owned());
        self.write();
        self
    }

    /// The fake exits with `code`.
    pub fn exits(mut self, code: i32) -> FakeCli {
        self.exit = code;
        self.write();
        self
    }

    /// The fake sleeps `secs` seconds after it recorded itself and printed.
    pub fn sleeps(mut self, secs: u32) -> FakeCli {
        self.sleep_secs = Some(secs);
        self.write();
        self
    }

    /// The fake starts `sleep secs` in the background, records the pid of that child in `<name>.child` and waits.
    pub fn spawns_a_sleeping_child(mut self, secs: u32) -> FakeCli {
        self.child_secs = Some(secs);
        self.write();
        self
    }

    /// The fake starts `sleep secs` in the background, records its pid in `<name>.child` and exits without waiting: the
    /// sleeping child keeps the fake's output pipes open.
    pub fn leaves_a_child(mut self, secs: u32) -> FakeCli {
        self.orphan_secs = Some(secs);
        self.write();
        self
    }

    /// The fake prints `n` bytes on standard output: the line `0123456789abcdef0123456789abcde` and a line feed,
    /// repeated (see [`pattern`]).
    pub fn prints_bytes(mut self, n: usize) -> FakeCli {
        self.out_bytes = n;
        self.write();
        self
    }

    /// The fake prints `n` bytes on standard error, in the same pattern.
    pub fn prints_stderr_bytes(mut self, n: usize) -> FakeCli {
        self.err_bytes = n;
        self.write();
        self
    }

    /// The fake runs `cat` on its standard input before anything else (it returns at once when the input is closed).
    pub fn reads_stdin(mut self) -> FakeCli {
        self.reads_stdin = true;
        self.write();
        self
    }

    /// The absolute path of the script.
    pub fn path(&self) -> PathBuf {
        self.dir.join(&self.name)
    }

    /// The arguments the fake received, one byte vector each.
    pub fn argv(&self) -> Vec<Vec<u8>> {
        let bytes = fs::read(self.file("argv")).expect("the fake did not run: no argv record");
        split_nul(&bytes)
    }

    /// Whether the fake ran at all (it recorded its arguments).
    pub fn started(&self) -> bool {
        self.file("argv").exists()
    }

    /// The environment the fake saw.
    pub fn env(&self) -> BTreeMap<String, String> {
        let bytes = fs::read(self.file("env")).expect("the fake did not run: no env record");
        split_nul(&bytes)
            .into_iter()
            .map(|entry| {
                let text = String::from_utf8_lossy(&entry).into_owned();
                match text.split_once('=') {
                    Some((name, value)) => (name.to_owned(), value.to_owned()),
                    None => (text, String::new()),
                }
            })
            .collect()
    }

    /// The raw bytes of the environment record, for a test that looks for a value anywhere.
    pub fn env_bytes(&self) -> Vec<u8> {
        fs::read(self.file("env")).expect("the fake did not run: no env record")
    }

    /// The pid of the sleeping child of `spawns_a_sleeping_child`, once the fake wrote it.
    pub fn child_pid(&self) -> Option<u32> {
        fs::read_to_string(self.file("child")).ok()?.trim().parse().ok()
    }

    /// The pid of the parent of the fake, as the fake saw it.
    pub fn parent_pid(&self) -> Option<u32> {
        fs::read_to_string(self.file("ppid")).ok()?.trim().parse().ok()
    }

    /// The process group of the fake, as the fake saw it.
    pub fn group_id(&self) -> Option<u32> {
        fs::read_to_string(self.file("pgid")).ok()?.trim().parse().ok()
    }

    fn file(&self, ext: &str) -> PathBuf {
        self.dir.join(format!("{}.{ext}", self.name))
    }

    /// Writes the files the script reads: the numbers and the texts to print.
    fn write(&self) {
        let conf = format!(
            "stdin={}\nout_bytes={}\nerr_bytes={}\nsleep_secs={}\nchild_secs={}\norphan_secs={}\nexit_code={}\n",
            u8::from(self.reads_stdin),
            self.out_bytes,
            self.err_bytes,
            self.sleep_secs.unwrap_or(0),
            self.child_secs.unwrap_or(0),
            self.orphan_secs.unwrap_or(0),
            self.exit
        );
        fs::write(self.file("conf"), conf).expect("write the settings");
        for (ext, text) in [("out", &self.out_text), ("err", &self.err_text)] {
            match text {
                Some(text) => fs::write(self.file(ext), text).expect("write the text"),
                None => {
                    let _ = fs::remove_file(self.file(ext));
                }
            }
        }
    }
}

/// The first `n` bytes of what `prints_bytes` and `prints_stderr_bytes` print.
pub fn pattern(n: usize) -> Vec<u8> {
    let line = b"0123456789abcdef0123456789abcde\n";
    line.iter().copied().cycle().take(n).collect()
}

/// Whether a process with this id exists (signal 0 through the system `kill`).
pub fn process_exists(pid: u32) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn split_nul(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut parts: Vec<Vec<u8>> = bytes.split(|b| *b == 0).map(<[u8]>::to_vec).collect();
    // The record ends with a NUL, so the last part is empty.
    if parts.last().is_some_and(Vec::is_empty) {
        parts.pop();
    }
    parts
}
