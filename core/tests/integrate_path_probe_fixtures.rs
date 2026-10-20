//! Hygiene of the captures of the login-shell `PATH` probe (`core/tests/fixtures/path_probe/`).
//!
//! The probe starts the user's login shell, prints a begin marker line, the value of `PATH` and an end marker line, and
//! the app keeps the directories of that value. A capture is what the probe wrote to standard output in one situation
//! (a clean startup file, a banner, an error, an odd list, a hang); its `.expected` file lists the directories the app must
//! keep, one per line. This file checks the fixtures themselves, with `std` only: every capture has its expected list, the
//! lists are absolute and bounded, no file carries a real home directory, and the table in `SOURCES.md` names every
//! capture. The comparison of a capture with its list is done by `scripts/probe-login-path.sh <shell> --check`, and by the
//! detector's own test later.
//!
//! Every rule is a small function that returns sentences, so a test can hand it a wrong directory and see it fail.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The begin and end marker lines of the probe output.
const BEGIN: &str = "@@hw-path-begin";
const END: &str = "@@hw-path-end";

/// The most directories an expected list may hold.
const MAX_DIRECTORIES: usize = 64;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn probe_dir() -> PathBuf {
    root().join("core/tests/fixtures/path_probe")
}

/// The case names (file stems) of the `.txt` captures in `dir`, sorted.
fn captures(dir: &Path) -> Vec<String> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("{} must exist: {e}", dir.display())).flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("txt") {
            names.insert(path.file_stem().expect("stem").to_string_lossy().into_owned());
        }
    }
    names.into_iter().collect()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The names below `/home/` that are not a person's home directory: the stand-in of the fixtures, and the account of a
/// Homebrew on Linux, whose install directory `/home/linuxbrew/.linuxbrew/bin` is a fixed public location.
const HOME_NAMES_ALLOWED: [&str; 2] = ["user", "linuxbrew"];

/// True when `text` names a home directory of a person: a macOS or Windows user folder, a Linux home other than the
/// names above, or a scratch directory that the recorder used.
fn home_path_in(text: &str) -> Option<String> {
    for line in text.lines() {
        for needle in ["/Users/", "\\Users\\", "/var/folders", "/private/var", "/tmp/tmp."] {
            if line.contains(needle) {
                return Some(line.to_string());
            }
        }
        let mut rest = line;
        while let Some(at) = rest.find("/home/") {
            let after = &rest[at + "/home/".len()..];
            let name: String = after.chars().take_while(|c| !matches!(c, '/' | ':' | ' ' | '"' | '\'' | '`' | ')')).collect();
            if !HOME_NAMES_ALLOWED.contains(&name.as_str()) {
                return Some(line.to_string());
            }
            rest = after;
        }
    }
    None
}

/// Every capture has an expected list, and every expected list has a capture. A capture that holds a begin marker also holds
/// an end marker after it; a capture that holds no marker line has an empty expected list.
fn pairing_problems(dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    let names = captures(dir);
    if names.is_empty() {
        problems.push(format!("{}: no capture found", dir.display()));
    }
    for name in &names {
        if !dir.join(format!("{name}.expected")).is_file() {
            problems.push(format!("{name}.txt has no {name}.expected"));
            continue;
        }
        let text = read(&dir.join(format!("{name}.txt")));
        let expected = read(&dir.join(format!("{name}.expected")));
        let lines: Vec<&str> = text.lines().collect();
        let begin = lines.iter().rposition(|l| *l == BEGIN);
        match begin {
            Some(b) => {
                if !lines[b + 1..].contains(&END) {
                    problems.push(format!("{name}.txt: a begin marker line with no end marker line after it"));
                }
            }
            None => {
                if !expected.trim().is_empty() {
                    problems.push(format!("{name}.txt has no begin marker line but {name}.expected is not empty"));
                }
            }
        }
        if lines.iter().any(|l| *l == END) && begin.is_none() {
            problems.push(format!("{name}.txt: an end marker line with no begin marker line"));
        }
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("expected") {
                let name = path.file_stem().expect("stem").to_string_lossy().into_owned();
                if !names.contains(&name) {
                    problems.push(format!("{name}.expected has no {name}.txt"));
                }
            }
        }
    }
    problems
}

/// The expected lists hold absolute directories only, at most 64, no duplicates, no empty line.
fn list_problems(dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for name in captures(dir) {
        let path = dir.join(format!("{name}.expected"));
        if !path.is_file() {
            continue;
        }
        let text = read(&path);
        let mut seen = BTreeSet::new();
        let mut count = 0;
        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            count += 1;
            if !line.starts_with('/') {
                problems.push(format!("{name}.expected:{number}: not an absolute directory: {line:?}"));
            }
            if line.contains(':') {
                problems.push(format!("{name}.expected:{number}: one directory per line, found a colon: {line:?}"));
            }
            if !seen.insert(line.to_string()) {
                problems.push(format!("{name}.expected:{number}: duplicate {line:?}"));
            }
            if line.contains("missing") {
                problems.push(format!("{name}.expected:{number}: a directory named missing is never kept: {line:?}"));
            }
        }
        if count > MAX_DIRECTORIES {
            problems.push(format!("{name}.expected holds {count} directories, the limit is {MAX_DIRECTORIES}"));
        }
    }
    problems
}

/// No file of the directory names a home directory other than the stand-in `/home/user`.
fn home_problems(dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("{} must exist: {e}", dir.display())).flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            problems.push(format!("{}: not UTF-8 text", path.display()));
            continue;
        };
        if let Some(line) = home_path_in(&text) {
            problems.push(format!("{}: names a real home or scratch path: {line:?}", path.file_name().unwrap().to_string_lossy()));
        }
    }
    problems
}

/// The cases the table of `SOURCES.md` names (the first column of the table under the heading `Captures`), with the
/// provenance column. A row is a line that starts with a pipe and is not the header or the rule.
fn table_cases(sources: &str) -> Vec<(String, String)> {
    let mut cases = Vec::new();
    let mut in_table = false;
    for line in sources.lines() {
        if line.starts_with("## ") {
            in_table = line.trim() == "## Captures";
            continue;
        }
        if !in_table || !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 3 || cells[0] == "Case" || cells[0].starts_with('-') {
            continue;
        }
        cases.push((cells[0].trim_matches('`').to_string(), cells[2].trim_matches('`').to_string()));
    }
    cases
}

/// The table of `SOURCES.md` and the captures are the same set, and each source is `recorded` or `written`.
fn sources_problems(dir: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    let sources = dir.join("SOURCES.md");
    if !sources.is_file() {
        return vec![format!("{}: missing", sources.display())];
    }
    let cases = table_cases(&read(&sources));
    let names = captures(dir);
    for (case, source) in &cases {
        if !names.contains(case) {
            problems.push(format!("SOURCES.md names the case {case} but {case}.txt does not exist"));
        }
        if source != "recorded" && source != "written" {
            problems.push(format!("SOURCES.md: the source of {case} is {source:?}, it must be recorded or written"));
        }
    }
    for name in &names {
        if !cases.iter().any(|(case, _)| case == name) {
            problems.push(format!("{name}.txt is not named in the table of SOURCES.md"));
        }
    }
    problems
}

#[test]
fn every_probe_capture_has_its_expected_list() {
    let problems = pairing_problems(&probe_dir());
    assert!(problems.is_empty(), "probe fixtures:\n{}", problems.join("\n"));
}

#[test]
fn expected_lists_hold_absolute_directories_only() {
    let problems = list_problems(&probe_dir());
    assert!(problems.is_empty(), "expected lists:\n{}", problems.join("\n"));
}

#[test]
fn no_probe_file_holds_a_home_path() {
    let problems = home_problems(&probe_dir());
    assert!(problems.is_empty(), "home paths in the probe fixtures:\n{}", problems.join("\n"));
}

#[test]
fn every_case_named_in_sources_md_has_a_capture() {
    let problems = sources_problems(&probe_dir());
    assert!(problems.is_empty(), "SOURCES.md:\n{}", problems.join("\n"));
}

#[test]
fn a_capture_with_no_markers_has_an_empty_expected_list() {
    // The slow startup file case: the shell is stopped before it prints anything.
    let dir = probe_dir();
    let capture = read(&dir.join("slow_rc.txt"));
    assert!(!capture.lines().any(|l| l == BEGIN || l == END), "the slow capture must hold no marker line");
    assert!(read(&dir.join("slow_rc.expected")).trim().is_empty(), "the slow case keeps no directory");
}

#[test]
fn the_rules_fail_for_a_wrong_directory() {
    let dir = common::scratch_dir("path-probe-rules");
    let put = |name: &str, text: &str| fs::write(dir.join(name), text).expect("write");

    // A capture without its expected file, and an expected file without its capture.
    put("a.txt", &format!("{BEGIN}\n/usr/bin\n{END}\n"));
    put("orphan.expected", "/usr/bin\n");
    let pairing = pairing_problems(&dir).join("\n");
    assert!(pairing.contains("a.txt has no a.expected"), "{pairing}");
    assert!(pairing.contains("orphan.expected has no orphan.txt"), "{pairing}");

    // A begin marker without an end marker, and an expected list for a capture without markers.
    put("b.txt", &format!("{BEGIN}\n/usr/bin\n"));
    put("b.expected", "/usr/bin\n");
    put("c.txt", "banner only\n");
    put("c.expected", "/usr/bin\n");
    let pairing = pairing_problems(&dir).join("\n");
    assert!(pairing.contains("b.txt: a begin marker line with no end marker"), "{pairing}");
    assert!(pairing.contains("c.txt has no begin marker line but c.expected is not empty"), "{pairing}");

    // A line that is not absolute, a duplicate, a colon and a directory that does not exist.
    put("d.txt", &format!("{BEGIN}\nx\n{END}\n"));
    put("d.expected", "bin\n/usr/bin\n/usr/bin\n/a:/b\n/home/user/missing1\n");
    let lists = list_problems(&dir).join("\n");
    assert!(lists.contains("d.expected:1: not an absolute directory"), "{lists}");
    assert!(lists.contains("d.expected:3: duplicate"), "{lists}");
    assert!(lists.contains("d.expected:4: one directory per line"), "{lists}");
    assert!(lists.contains("d.expected:5: a directory named missing"), "{lists}");

    // More than 64 directories.
    let many: String = (0..65).map(|n| format!("/opt/d{n}\n")).collect();
    put("e.txt", &format!("{BEGIN}\nx\n{END}\n"));
    put("e.expected", &many);
    assert!(list_problems(&dir).join("\n").contains("e.expected holds 65 directories"));

    // A home path other than the stand-in.
    put("f.txt", "/home/user/bin\n/home/christoph/bin\n");
    put("g.txt", "/Users/someone/bin\n");
    put("h.txt", "/home/user/bin:/home/user/.local/bin:/home/linuxbrew/.linuxbrew/bin\n");
    let homes = home_problems(&dir).join("\n");
    assert!(homes.contains("f.txt"), "{homes}");
    assert!(homes.contains("g.txt"), "{homes}");
    assert!(!homes.contains("h.txt"), "{homes}");

    // The table and the captures are the same set, and the source is recorded or written.
    put(
        "SOURCES.md",
        "# t\n\n## Captures\n\n| Case | Shell | Source | Note |\n|---|---|---|---|\n| a | zsh | recorded | x |\n| ghost | zsh | made up | x |\n\n## Other\n\n| zzz | y | w | x |\n",
    );
    let sources = sources_problems(&dir).join("\n");
    assert!(sources.contains("names the case ghost but ghost.txt does not exist"), "{sources}");
    assert!(sources.contains("the source of ghost is \"made up\""), "{sources}");
    assert!(sources.contains("b.txt is not named in the table"), "{sources}");
    assert!(!sources.contains("zzz"), "{sources}");

    fs::remove_dir_all(&dir).expect("remove scratch");
}
