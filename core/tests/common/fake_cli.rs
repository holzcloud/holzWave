//! The fake program rig of the integration tests: a script that stands in for a command-line tool.
//!
//! A fake records what it was started with, then behaves as chosen. The recording goes to files next to the script
//! (`<name>.argv`: the arguments as NUL-terminated bytes, `<name>.env`: the environment as NUL-terminated
//! `NAME=VALUE` entries, `<name>.ppid`, `<name>.pgid`, `<name>.child`), so a test reads back exactly what the program
//! received. The text of the script is written by this file only: nothing a test passes (output text, counts, a
//! number of seconds) is interpolated into it; outputs go through files the rig writes and the script copies.
//!
//! The scripts are POSIX shell, so the rig is Unix only.
#![allow(dead_code)]
#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A fake program in a scratch directory. Every builder method rewrites the script, so a test can chain them.
#[derive(Debug, Clone)]
pub struct FakeCli {
    dir: PathBuf,
    name: String,
    reads_stdin: bool,
    out_bytes: usize,
    err_bytes: usize,
    sleep_secs: Option<u32>,
    child_secs: Option<u32>,
    exit: i32,
    out_text: Option<String>,
    err_text: Option<String>,
}

impl FakeCli {
    /// A fake named `name` in `dir`: it records its start and exits with status 0 without output.
    pub fn new(dir: &Path, name: &str) -> FakeCli {
        assert!(!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
        let shown = dir.to_string_lossy();
        assert!(!shown.contains('\'') && !shown.contains('\n'), "the scratch directory must be plain");
        let fake = FakeCli {
            dir: dir.to_path_buf(),
            name: name.to_owned(),
            reads_stdin: false,
            out_bytes: 0,
            err_bytes: 0,
            sleep_secs: None,
            child_secs: None,
            exit: 0,
            out_text: None,
            err_text: None,
        };
        fake.write();
        fake
    }

    /// The fake prints `text` on standard output (exactly these bytes).
    pub fn prints(mut self, text: &str) -> FakeCli {
        self.out_text = Some(text.to_owned());
        fs::write(self.file("out"), text).expect("write the output text");
        self.write();
        self
    }

    /// The fake prints `text` on standard error.
    pub fn prints_stderr(mut self, text: &str) -> FakeCli {
        self.err_text = Some(text.to_owned());
        fs::write(self.file("err"), text).expect("write the error text");
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

    /// The fake starts `sleep secs` in the background, records the pid of that child in `<name>.child` and waits for it.
    pub fn spawns_a_sleeping_child(mut self, secs: u32) -> FakeCli {
        self.child_secs = Some(secs);
        self.write();
        self
    }

    /// The fake prints `n` bytes on standard output: the line `0123456789abcdef0123456789abcde` and a line feed, repeated
    /// (see [`pattern`]).
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

    fn write(&self) {
        let mut script = String::from("#!/bin/sh\n");
        script.push_str(&format!("d='{}'\n", self.dir.to_string_lossy()));
        let n = &self.name;
        script.push_str(&format!("for a in \"$@\"; do printf '%s\\0' \"$a\"; done > \"$d/{n}.argv\"\n"));
        script.push_str(&format!("/usr/bin/env -0 > \"$d/{n}.env\"\n"));
        script.push_str(&format!("echo \"$PPID\" > \"$d/{n}.ppid\"\n"));
        script.push_str(&format!("/bin/ps -o pgid= -p \"$$\" | tr -d ' ' > \"$d/{n}.pgid\"\n"));
        if self.reads_stdin {
            script.push_str("cat\n");
        }
        if self.out_text.is_some() {
            script.push_str(&format!("cat \"$d/{n}.out\"\n"));
        }
        if self.err_text.is_some() {
            script.push_str(&format!("cat \"$d/{n}.err\" >&2\n"));
        }
        if self.out_bytes > 0 {
            script.push_str(&format!("yes 0123456789abcdef0123456789abcde | head -c {}\n", self.out_bytes));
        }
        if self.err_bytes > 0 {
            script.push_str(&format!("yes 0123456789abcdef0123456789abcde | head -c {} >&2\n", self.err_bytes));
        }
        if let Some(secs) = self.child_secs {
            script.push_str(&format!("sleep {secs} &\necho \"$!\" > \"$d/{n}.child\"\nwait\n"));
        }
        if let Some(secs) = self.sleep_secs {
            script.push_str(&format!("sleep {secs}\n"));
        }
        script.push_str(&format!("exit {}\n", self.exit));
        let path = self.path();
        fs::write(&path, script).expect("write the fake");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("make the fake executable");
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
