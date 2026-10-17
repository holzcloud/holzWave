//! The app links Apple's model framework weakly, so it starts on a Mac whose system has no such framework (macOS below 26).
//!
//! The bridge crate links the framework weakly for its own tests, but a library crate's link arguments do not reach the crates
//! that depend on it: the app's build script repeats them. The parser below reads the load commands of `otool -l`; the macOS
//! test applies it to the executable it is running as, which is linked with the same arguments as the app. Another host has
//! the parser tests only. This file contains no foreign-function code.

/// The load commands of `otool -l` output that carry a dylib name, as `(command, name)` in file order. `LC_ID_DYLIB`,
/// `LC_RPATH`, `LC_LOAD_DYLINKER` and every command without a dylib name are not among them; a `name` line under a command
/// that does not load a library is ignored.
fn dylib_entries(otool_output: &str) -> Vec<(String, String)> {
    const LOADING: [&str; 5] =
        ["LC_LOAD_DYLIB", "LC_LOAD_WEAK_DYLIB", "LC_REEXPORT_DYLIB", "LC_LOAD_UPWARD_DYLIB", "LC_LAZY_LOAD_DYLIB"];
    let mut entries = Vec::new();
    let mut command = String::new();
    for line in otool_output.lines() {
        let line = line.trim();
        if line.starts_with("Load command") {
            command.clear();
        } else if let Some(name) = line.strip_prefix("cmd ") {
            command = name.trim().to_owned();
        } else if let Some(rest) = line.strip_prefix("name ") {
            if LOADING.contains(&command.as_str()) {
                let name = rest.split(" (offset").next().unwrap_or(rest).trim();
                entries.push((command.clone(), name.to_owned()));
            }
        }
    }
    entries
}

/// Whether a command loads the library only if it exists.
fn is_weak(command: &str) -> bool {
    command == "LC_LOAD_WEAK_DYLIB"
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
Load command 12
          cmd LC_REEXPORT_DYLIB
         name /usr/lib/a.dylib (offset 24)
Load command 13
          cmd LC_LOAD_UPWARD_DYLIB
         name /usr/lib/b.dylib (offset 24)
Load command 14
          cmd LC_LAZY_LOAD_DYLIB
         name /usr/lib/c.dylib (offset 24)
";

#[test]
fn the_parser_separates_weak_from_strong_dylibs() {
    let entries = dylib_entries(CANNED);
    let weak: Vec<&str> = entries.iter().filter(|(command, _)| is_weak(command)).map(|(_, name)| name.as_str()).collect();
    let strong: Vec<&str> = entries.iter().filter(|(command, _)| !is_weak(command)).map(|(_, name)| name.as_str()).collect();
    assert_eq!(weak, ["/System/Library/Frameworks/FoundationModels.framework/Versions/A/FoundationModels"]);
    // An entry under the load, re-export, upward and lazy commands is strong.
    assert_eq!(strong, ["/usr/lib/libSystem.B.dylib", "/usr/lib/a.dylib", "/usr/lib/b.dylib", "/usr/lib/c.dylib"]);
}

#[test]
fn the_parser_ignores_commands_without_a_dylib_name() {
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
    assert_eq!(dylib_entries(text), Vec::<(String, String)>::new());
    assert_eq!(dylib_entries(""), Vec::<(String, String)>::new());
}

#[cfg(target_os = "macos")]
#[test]
fn foundation_models_is_weak_linked_in_this_executable() {
    use holzwave_apple_ai::{Availability, probe};

    // Call the bridge first, so that the linker keeps it in the executable.
    let availability = std::hint::black_box(probe());
    let exe = std::env::current_exe().expect("the path of the test executable");
    let output = std::process::Command::new("otool").arg("-l").arg(&exe).output().expect("otool runs");
    assert!(output.status.success(), "otool -l failed: {}", String::from_utf8_lossy(&output.stderr));
    let entries = dylib_entries(&String::from_utf8_lossy(&output.stdout));
    let named: Vec<&(String, String)> =
        entries.iter().filter(|(_, name)| name.contains("/FoundationModels.framework/")).collect();
    for (command, name) in &named {
        println!("{command} {name}");
    }
    if availability == Availability::SdkLacksFramework {
        // An SDK without the framework builds an executable that names it nowhere.
        assert!(named.is_empty(), "the SDK lacks the framework, yet the executable names it: {named:?}");
    } else {
        let weak = named.iter().filter(|(command, _)| is_weak(command)).count();
        let strong = named.len() - weak;
        assert_eq!(strong, 0, "FoundationModels is linked strongly: {named:?}");
        assert_eq!(weak, 1, "expected exactly one weak entry for FoundationModels: {named:?}");
    }
}
