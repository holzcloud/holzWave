//! Guards over the sources of the crate: where foreign calls may live, that library code prints nothing, and that both
//! sides of the C ABI name the same functions. Each scan is a small pure function over `(label, source)` pairs, with a
//! test that shows it can fail.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// The part of a source that ships: everything before its declared test module.
fn production_part(source: &str) -> &str {
    source.split("\n#[cfg(test)]\nmod ").next().unwrap_or(source)
}

/// The code lines of a source that ships, with their numbers: comment lines are skipped.
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

/// The files of `src` that may hold foreign calls.
const FFI_FILES: [&str; 2] = ["lib.rs", "native.rs"];
/// How often the attribute that lets a module hold foreign calls may appear in the sources.
const ALLOW_ATTRIBUTES: usize = 2;

/// `unsafe` outside the two foreign-call files, and the number of allow attributes in all sources.
fn unsafe_problems(sources: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut allows = 0;
    for (label, source) in sources {
        for (number, line) in code_lines(source) {
            if line.contains("allow(unsafe_code)") {
                allows += 1;
            }
            if has_token(line, "unsafe") && !FFI_FILES.iter().any(|file| label.ends_with(file)) {
                problems.push(format!("{label}:{number}: unsafe outside the foreign-call files"));
            }
        }
    }
    if allows != ALLOW_ATTRIBUTES {
        problems.push(format!("allow(unsafe_code) appears {allows} times, expected {ALLOW_ATTRIBUTES}"));
    }
    problems
}

const PRINT_TOKENS: [&str; 6] = ["println!", "eprintln!", "print!", "eprint!", "dbg!", "todo!"];

/// Console output and placeholders in the code that ships.
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

const ABI_PREFIX: &str = "hw_afm_";

/// The `hw_afm_` names after `marker` on the code lines of a source.
fn abi_names_after(source: &str, marker: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for (_, line) in code_lines(source) {
        let mut rest = line;
        while let Some(found) = rest.find(marker) {
            rest = &rest[found + marker.len()..];
            let trimmed = rest.trim_start_matches(['(', '"', ' ']);
            if let Some(tail) = trimmed.strip_prefix(ABI_PREFIX) {
                let end = tail.find(|c: char| !is_word(c)).unwrap_or(tail.len());
                names.insert(format!("{ABI_PREFIX}{}", &tail[..end]));
            }
        }
    }
    names
}

/// Every name declared in Rust must be defined in Swift and the reverse. Rust declares with `fn hw_afm_...`; Swift
/// defines with `@_cdecl("hw_afm_...")`.
fn abi_problems(rust: &[(String, String)], swift: &[(String, String)]) -> Vec<String> {
    let declared: BTreeSet<String> = rust.iter().flat_map(|(_, source)| abi_names_after(source, "fn ")).collect();
    let defined: BTreeSet<String> = swift.iter().flat_map(|(_, source)| abi_names_after(source, "@_cdecl")).collect();
    let mut problems = Vec::new();
    for name in declared.difference(&defined) {
        problems.push(format!("{name} is declared in Rust and defined by no @_cdecl in Swift"));
    }
    for name in defined.difference(&declared) {
        problems.push(format!("{name} is defined in Swift and declared nowhere in Rust"));
    }
    problems
}

fn read_dir_files(dir: &Path, extension: &str) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut paths: Vec<_> = fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display())).flatten().collect();
    paths.sort_by_key(|entry| entry.path());
    for entry in paths {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some(extension) {
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            files.push((path.file_name().unwrap().to_string_lossy().into_owned(), text));
        }
    }
    files
}

fn crate_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn rust_sources() -> Vec<(String, String)> {
    let files = read_dir_files(&crate_dir().join("src"), "rs");
    assert!(files.len() >= 7, "only {} Rust files read: is the walk broken?", files.len());
    files
}

fn swift_sources() -> Vec<(String, String)> {
    let files = read_dir_files(&crate_dir().join("swift/Sources/AppleAI"), "swift");
    assert!(files.len() >= 3, "only {} Swift files read: is the walk broken?", files.len());
    files
}

fn pair(label: &str, source: &str) -> (String, String) {
    (label.to_owned(), source.to_owned())
}

#[test]
fn unsafe_code_lives_only_in_the_ffi_files() {
    let problems = unsafe_problems(&rust_sources());
    assert!(problems.is_empty(), "foreign calls leaked:\n  {}", problems.join("\n  "));
}

#[test]
fn the_unsafe_scan_can_fail() {
    let leak = [pair("lib.rs", "#[allow(unsafe_code)]\nmod a {}\n#[allow(unsafe_code)]\nmod b {}\n"), pair("pump.rs", "fn f() {\n    unsafe { g() }\n}\n")];
    let problems = unsafe_problems(&leak);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].starts_with("pump.rs:2:"), "{problems:?}");

    let third = [pair("lib.rs", "#[allow(unsafe_code)]\n#[allow(unsafe_code)]\n#[allow(unsafe_code)]\n")];
    assert_eq!(unsafe_problems(&third).len(), 1);
    let one = [pair("lib.rs", "#[allow(unsafe_code)]\n")];
    assert_eq!(unsafe_problems(&one).len(), 1, "too few allow attributes are reported too");

    // Words that only contain the name, comments and the test module are not code that ships.
    let fine = [
        pair("lib.rs", "#![deny(unsafe_code)]\n#[allow(unsafe_code)]\n#[allow(unsafe_code)]\n"),
        pair("pump.rs", "// unsafe in a comment\n/// unsafe in a doc comment\nfn unsafe_free() {}\n#[cfg(test)]\nmod tests {\n    unsafe {}\n}\n"),
    ];
    assert_eq!(unsafe_problems(&fine), Vec::<String>::new());
}

#[test]
fn library_sources_print_nothing() {
    let problems = print_problems(&rust_sources());
    assert!(problems.is_empty(), "library sources print:\n  {}", problems.join("\n  "));
}

#[test]
fn the_print_scan_can_fail() {
    let bad = [pair(
        "provider.rs",
        "fn a() { println!(\"x\"); }\nfn b() { eprintln!(\"x\"); }\nfn c() { print!(\"x\"); }\nfn d() { eprint!(\"x\"); }\nfn e() { dbg!(1); }\nfn f() { todo!() }\n",
    )];
    let problems = print_problems(&bad);
    for token in ["println!", "eprintln!", "print!", "eprint!", "dbg!", "todo!"] {
        assert!(problems.iter().any(|p| p.ends_with(token)), "{token} not reported: {problems:?}");
    }

    let fine = [pair("a.rs", "// println!(\"x\")\nfn sprint() {}\nfn f() { format!(\"x\"); }\n#[cfg(test)]\nmod tests {\n    fn t() { println!(\"x\"); }\n}\n")];
    assert_eq!(print_problems(&fine), Vec::<String>::new());
}

#[test]
fn the_c_abi_names_agree_on_both_sides() {
    let rust = rust_sources();
    let swift = swift_sources();
    let problems = abi_problems(&rust, &swift);
    assert!(problems.is_empty(), "the two sides of the C ABI disagree:\n  {}", problems.join("\n  "));
    let declared: BTreeSet<String> = rust.iter().flat_map(|(_, source)| abi_names_after(source, "fn ")).collect();
    assert!(declared.len() >= 6, "only {} names found in Rust: is the scan broken? {declared:?}", declared.len());
}

#[test]
fn the_abi_scan_can_fail() {
    let rust = [pair("native.rs", "unsafe extern \"C\" {\n    fn hw_afm_one() -> i32;\n    fn hw_afm_two(x: i32);\n}\nextern \"C\" fn on_event() {}\n")];
    let swift = [pair("A.swift", "@_cdecl(\"hw_afm_one\")\npublic func hw_afm_one() -> Int32 { 1 }\n@_cdecl(\"hw_afm_three\")\npublic func hw_afm_three() {}\n")];

    let problems = abi_problems(&rust, &swift);

    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("hw_afm_two") && p.contains("Rust")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("hw_afm_three") && p.contains("Swift")), "{problems:?}");

    let matching = [pair("A.swift", "@_cdecl(\"hw_afm_one\")\n@_cdecl(\"hw_afm_two\")\n")];
    assert_eq!(abi_problems(&rust, &matching), Vec::<String>::new());
}
