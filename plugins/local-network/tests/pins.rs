//! Guards over the sources of the crate: that the Swift source, the Rust constant and the app's `Info.plist` name the same
//! Bonjour type, where foreign calls may live, that library code prints nothing, and that the Swift package stays small.
//! Each scan is a small pure function with a test that shows it can fail. No test here calls the real probe.

use std::fs;
use std::path::Path;

use holzwave_local_network::SERVICE_TYPE;

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The part of a source that ships: everything before its declared test module.
fn production_part(source: &str) -> &str {
    source.split("\n#[cfg(test)]\nmod ").next().unwrap_or(source)
}

/// The code lines of a source that ships: comment lines are skipped.
fn code_lines(source: &str) -> impl Iterator<Item = (usize, &str)> {
    production_part(source)
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line))
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Whether `line` holds `token` as a whole word (`unsafe` is not found in `unsafe_code`) or macro name.
fn has_token(line: &str, token: &str) -> bool {
    let mut from = 0;
    while let Some(found) = line[from..].find(token) {
        let start = from + found;
        let end = start + token.len();
        let before_ok = line[..start].chars().next_back().is_none_or(|c| !is_word(c));
        let after_ok = token.ends_with('!') || line[end..].chars().next().is_none_or(|c| !is_word(c));
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

fn pair(label: &str, source: &str) -> (String, String) {
    (label.to_owned(), source.to_owned())
}

fn rust_sources() -> Vec<(String, String)> {
    let mut files = Vec::new();
    for entry in fs::read_dir(crate_dir().join("src")).expect("src is readable").flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push((path.file_name().unwrap().to_string_lossy().into_owned(), read(&path)));
        }
    }
    files.sort();
    assert!(files.len() >= 2, "only {} Rust files read: is the walk broken?", files.len());
    files
}

// ---- one service type in three places ----------------------------------------------------------------------

/// The string literal assigned to `serviceType` in the Swift source.
fn swift_service_type(source: &str) -> Option<String> {
    let line = source.lines().find(|line| line.contains("let serviceType"))?;
    let start = line.find('"')? + 1;
    let end = start + line[start..].find('"')?;
    Some(line[start..end].to_owned())
}

/// The strings of the array that follows `NSBonjourServices` in a plist.
fn plist_bonjour_services(plist: &str) -> Vec<String> {
    let Some(after) = plist.split("<key>NSBonjourServices</key>").nth(1) else { return Vec::new() };
    let Some(array) = after.split("<array>").nth(1).and_then(|rest| rest.split("</array>").next()) else {
        return Vec::new();
    };
    array
        .split("<string>")
        .skip(1)
        .filter_map(|part| part.split("</string>").next())
        .map(str::to_owned)
        .collect()
}

#[test]
fn swift_rust_and_the_plist_name_the_same_service_type() {
    let swift = read(&crate_dir().join("swift/Sources/LocalNetwork/Probe.swift"));
    let plist = read(&crate_dir().join("../../app/src-tauri/Info.plist"));
    assert_eq!(swift_service_type(&swift).as_deref(), Some(SERVICE_TYPE), "Swift and the Rust constant differ");
    assert_eq!(plist_bonjour_services(&plist), vec![SERVICE_TYPE.to_owned()], "Info.plist and the constant differ");
    assert!(SERVICE_TYPE.starts_with('_') && SERVICE_TYPE.ends_with("._tcp"), "{SERVICE_TYPE} is not a Bonjour type");
}

#[test]
fn the_service_type_scans_can_fail() {
    assert_eq!(swift_service_type("private let serviceType = \"_a._tcp\"\n").as_deref(), Some("_a._tcp"));
    assert_eq!(swift_service_type("let other = \"x\"\n"), None);
    let plist = "<key>NSBonjourServices</key>\n<array>\n<string>_a._tcp</string>\n<string>_b._tcp</string>\n</array>";
    assert_eq!(plist_bonjour_services(plist), vec!["_a._tcp".to_owned(), "_b._tcp".to_owned()]);
    assert!(plist_bonjour_services("<key>Other</key>").is_empty());
}

// ---- where foreign calls live --------------------------------------------------------------------------------

/// The one file that may hold foreign calls.
const FFI_FILE: &str = "system.rs";

/// `unsafe` outside the foreign-call file, and the number of allow attributes in all sources (exactly one: the module).
fn unsafe_problems(sources: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut allows = 0;
    for (label, source) in sources {
        for (number, line) in code_lines(source) {
            if line.contains("allow(unsafe_code)") {
                allows += 1;
            }
            if has_token(line, "unsafe") && label != FFI_FILE {
                problems.push(format!("{label}:{number}: unsafe outside {FFI_FILE}"));
            }
        }
    }
    if allows != 1 {
        problems.push(format!("allow(unsafe_code) appears {allows} times, expected 1"));
    }
    problems
}

#[test]
fn unsafe_code_lives_only_in_system_rs() {
    let problems = unsafe_problems(&rust_sources());
    assert!(problems.is_empty(), "foreign calls leaked:\n  {}", problems.join("\n  "));
    let system = rust_sources().into_iter().find(|(label, _)| label == FFI_FILE).expect("system.rs exists");
    assert!(code_lines(&system.1).any(|(_, line)| has_token(line, "unsafe")), "system.rs holds no foreign call: is the scan broken?");
}

#[test]
fn the_unsafe_scan_can_fail() {
    let leak = [pair("lib.rs", "#[allow(unsafe_code)]\nmod system;\n"), pair("other.rs", "fn f() {\n    unsafe { g() }\n}\n")];
    let problems = unsafe_problems(&leak);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("other.rs:2:"), "{problems:?}");
    let none = [pair("lib.rs", "mod system;\n")];
    assert_eq!(unsafe_problems(&none).len(), 1, "a missing allow attribute is reported too");
    let two = [pair("lib.rs", "#[allow(unsafe_code)]\n#[allow(unsafe_code)]\n")];
    assert_eq!(unsafe_problems(&two).len(), 1);

    // Words that only contain the name, comments and the test module are not code that ships.
    let fine = [
        pair("lib.rs", "#![deny(unsafe_code)]\n#[allow(unsafe_code)]\nmod system;\n"),
        pair("system.rs", "unsafe extern \"C\" {}\n"),
        pair("other.rs", "// unsafe in a comment\n/// unsafe in a doc comment\nfn unsafe_free() {}\n#[cfg(test)]\nmod tests {\n    unsafe {}\n}\n"),
    ];
    assert_eq!(unsafe_problems(&fine), Vec::<String>::new());
}

// ---- library code prints nothing -----------------------------------------------------------------------------

const PRINT_TOKENS: [&str; 6] = ["println!", "eprintln!", "print!", "eprint!", "dbg!", "todo!"];

fn print_problems(sources: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (label, source) in sources {
        for (number, line) in code_lines(source) {
            for token in PRINT_TOKENS {
                if has_token(line, token) {
                    problems.push(format!("{label}:{number}: {token}"));
                }
            }
        }
    }
    problems
}

#[test]
fn library_sources_print_nothing() {
    let problems = print_problems(&rust_sources());
    assert!(problems.is_empty(), "library sources print:\n  {}", problems.join("\n  "));
}

#[test]
fn the_print_scan_can_fail() {
    let bad = [pair("lib.rs", "fn a() { println!(\"x\"); }\nfn b() { eprintln!(\"x\"); }\nfn c() { dbg!(1); }\n")];
    let problems = print_problems(&bad);
    for token in ["println!", "eprintln!", "dbg!"] {
        assert!(problems.iter().any(|p| p.ends_with(token)), "{token} not reported: {problems:?}");
    }
    let fine = [pair("a.rs", "// println!(\"x\")\nfn sprint() {}\n#[cfg(test)]\nmod tests {\n    fn t() { println!(\"x\"); }\n}\n")];
    assert_eq!(print_problems(&fine), Vec::<String>::new());
}

// ---- the Swift package ---------------------------------------------------------------------------------------

/// Words that would put a dependency, a binary or a plugin into the package.
const PACKAGE_FORBIDDEN: [&str; 4] = [".package(", ".binaryTarget", ".plugin", "dependencies:"];

fn package_problems(manifest: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (number, line) in manifest.lines().enumerate() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for word in PACKAGE_FORBIDDEN {
            if line.contains(word) {
                problems.push(format!("Package.swift:{}: {word}", number + 1));
            }
        }
    }
    problems
}

#[test]
fn the_swift_package_has_no_dependency_binary_or_plugin() {
    let manifest = read(&crate_dir().join("swift/Package.swift"));
    let problems = package_problems(&manifest);
    assert!(problems.is_empty(), "the package grew:\n  {}", problems.join("\n  "));
    assert!(manifest.contains("swift-tools-version"), "is this the manifest?");
}

#[test]
fn the_package_scan_can_fail() {
    let bad = "let package = Package(\n  dependencies: [.package(url: \"x\", from: \"1\")],\n  targets: [.binaryTarget(name: \"b\")]\n)\n";
    assert!(package_problems(bad).len() >= 3);
    assert_eq!(package_problems("// dependencies: none\nlet package = Package(name: \"A\")\n"), Vec::<String>::new());
}

#[test]
fn the_macos_floor_is_the_same_in_the_package_and_the_build_script() {
    let build = read(&crate_dir().join("build.rs"));
    let manifest = read(&crate_dir().join("swift/Package.swift"));
    assert!(build.contains("const MACOS_FLOOR: &str = \"13.0\""));
    assert!(manifest.contains(".macOS(.v13)"));
}

// ---- the symbols link ----------------------------------------------------------------------------------------

#[test]
fn both_foreign_functions_link_without_being_called() {
    assert!(holzwave_local_network::symbols_link());
}
