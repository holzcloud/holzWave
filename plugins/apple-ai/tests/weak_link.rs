//! The bridge must not be a hard dependency of the process: an app that contains it has to start on a Mac whose
//! operating system has no FoundationModels framework. These tests read the load commands of the test executable
//! itself, check that a Swift task calls back into Rust exactly once per call, and pin the deployment floor.

/// The dylib names of the load commands of `otool -l` output: (weak, strong). Weak is `LC_LOAD_WEAK_DYLIB`; strong is
/// `LC_LOAD_DYLIB`, `LC_REEXPORT_DYLIB`, `LC_LOAD_UPWARD_DYLIB` and `LC_LAZY_LOAD_DYLIB`. `LC_ID_DYLIB`, `LC_RPATH` and
/// every command without a dylib name are ignored.
fn dylib_commands(otool_output: &str) -> (Vec<String>, Vec<String>) {
    let mut weak = Vec::new();
    let mut strong = Vec::new();
    let mut command = String::new();
    for line in otool_output.lines() {
        let line = line.trim();
        if line.starts_with("Load command") {
            command.clear();
        } else if let Some(name) = line.strip_prefix("cmd ") {
            command = name.trim().to_owned();
        } else if let Some(rest) = line.strip_prefix("name ") {
            let name = rest.split(" (offset").next().unwrap_or(rest).trim().to_owned();
            match command.as_str() {
                "LC_LOAD_WEAK_DYLIB" => weak.push(name),
                "LC_LOAD_DYLIB" | "LC_REEXPORT_DYLIB" | "LC_LOAD_UPWARD_DYLIB" | "LC_LAZY_LOAD_DYLIB" => {
                    strong.push(name)
                }
                _ => {}
            }
        }
    }
    (weak, strong)
}

const CANNED: &str = "\
Load command 10
          cmd LC_LOAD_DYLIB
      cmdsize 56
         name /usr/lib/libSystem.B.dylib (offset 24)
   time stamp 2 Thu Jan  1 01:00:02 1970
Load command 11
          cmd LC_LOAD_WEAK_DYLIB
      cmdsize 96
         name /System/Library/Frameworks/FoundationModels.framework/Versions/A/FoundationModels (offset 24)
   time stamp 2 Thu Jan  1 01:00:02 1970
";

#[test]
fn parser_separates_weak_from_strong_dylibs() {
    let (weak, strong) = dylib_commands(CANNED);
    assert_eq!(weak, ["/System/Library/Frameworks/FoundationModels.framework/Versions/A/FoundationModels"]);
    assert_eq!(strong, ["/usr/lib/libSystem.B.dylib"]);
}

#[test]
fn parser_counts_reexport_upward_and_lazy_as_strong() {
    let text = "\
Load command 1
          cmd LC_REEXPORT_DYLIB
         name /usr/lib/a.dylib (offset 24)
Load command 2
          cmd LC_LOAD_UPWARD_DYLIB
         name /usr/lib/b.dylib (offset 24)
Load command 3
          cmd LC_LAZY_LOAD_DYLIB
         name /usr/lib/c.dylib (offset 24)
";
    let (weak, strong) = dylib_commands(text);
    assert!(weak.is_empty());
    assert_eq!(strong, ["/usr/lib/a.dylib", "/usr/lib/b.dylib", "/usr/lib/c.dylib"]);
}

#[test]
fn parser_ignores_commands_without_a_dylib_name() {
    let text = "\
Load command 1
          cmd LC_ID_DYLIB
         name /usr/lib/self.dylib (offset 24)
Load command 2
          cmd LC_RPATH
      cmdsize 40
         path /usr/lib/swift (offset 12)
Load command 3
          cmd LC_LOAD_DYLINKER
         name /usr/lib/dyld (offset 12)
Load command 4
          cmd LC_MAIN
     entryoff 1234
";
    let (weak, strong) = dylib_commands(text);
    assert!(weak.is_empty() && strong.is_empty(), "weak {weak:?}, strong {strong:?}");
}

/// The number after `marker` up to the first dot or quote: the major version of the deployment floor.
fn major_after(text: &str, marker: &str) -> u32 {
    let rest = text.split(marker).nth(1).unwrap_or_else(|| panic!("{marker:?} not found"));
    let digits: String = rest.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or_else(|_| panic!("no number after {marker:?}"))
}

#[test]
fn deployment_floor_is_one_value() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let build = std::fs::read_to_string(dir.join("build.rs")).expect("build.rs is readable");
    let package = std::fs::read_to_string(dir.join("swift/Package.swift")).expect("Package.swift is readable");
    let floor = major_after(&build, "const MACOS_FLOOR");
    assert_eq!(floor, major_after(&package, ".macOS(.v"), "build.rs and Package.swift name different floors");
    assert!(floor >= 11, "implausible floor {floor}");
}

#[cfg(target_os = "macos")]
mod mac {
    use super::dylib_commands;
    use holzwave_apple_ai::{SELFTEST_VALUE, probe, selftest};
    use std::time::{Duration, Instant};

    #[test]
    fn foundation_models_is_weak_linked_in_this_executable() {
        // Call the bridge first, so that the linker keeps it in the executable.
        println!("probe: {}", std::hint::black_box(probe()).as_str());
        let exe = std::env::current_exe().expect("the path of the test executable");
        let output = std::process::Command::new("otool").arg("-l").arg(&exe).output().expect("otool runs");
        assert!(output.status.success(), "otool -l failed: {}", String::from_utf8_lossy(&output.stderr));
        let text = String::from_utf8_lossy(&output.stdout);
        let (weak, strong) = dylib_commands(&text);
        let is_framework = |name: &&String| name.contains("FoundationModels.framework");
        let weak_entries: Vec<&String> = weak.iter().filter(is_framework).collect();
        let strong_entries: Vec<&String> = strong.iter().filter(is_framework).collect();
        for name in &weak_entries {
            println!("LC_LOAD_WEAK_DYLIB {name}");
        }
        assert!(strong_entries.is_empty(), "FoundationModels is linked strongly: {strong_entries:?}");
        if cfg!(apple_ai_sdk_foundation_models) {
            assert_eq!(weak_entries.len(), 1, "expected one weak entry for FoundationModels in {weak:?}");
        } else {
            assert!(weak_entries.is_empty(), "the SDK lacks the framework, yet the executable names it: {weak:?}");
        }
    }

    #[test]
    fn selftest_calls_back_once_per_call_from_a_swift_task() {
        let start = Instant::now();
        for round in 0..50 {
            assert_eq!(selftest(Duration::from_secs(5)), Some(SELFTEST_VALUE), "round {round}");
            // `selftest` returns `None` when a second callback shows up within its own grace period; this wait makes
            // a late duplicate visible as a crash or a hang of the next round rather than as silence.
            std::thread::sleep(Duration::from_millis(20));
        }
        println!("50 round trips in {:?}", start.elapsed());
    }
}
