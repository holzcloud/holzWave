//! Text tests of the committed Android project (`gen/android`): the application id, the minimum SDK, the one ABI, no machine
//! path and no key, the permissions the app asks for, and the build types. They read files only: no Android tool is needed,
//! so they run on the desktop in `cargo test`.
//!
//! Every check is a small pure function that returns a list of problems, one sentence each, and every function has a test
//! on a bad input, so a check that can never fail is caught here. If a generated file contradicts a check, the file of the
//! project is fixed, not the check.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const TAURI_CONF: &str = include_str!("../tauri.conf.json");
const ANDROID_CONF: &str = include_str!("../tauri.android.conf.json");
const APP_GRADLE: &str = include_str!("../gen/android/app/build.gradle.kts");
const MANIFEST: &str = include_str!("../gen/android/app/src/main/AndroidManifest.xml");
const RUST_PLUGIN: &str = include_str!("../gen/android/buildSrc/src/main/java/ch/holzcloud/holzwave/kotlin/RustPlugin.kt");
const MAIN_ACTIVITY: &str = include_str!("../gen/android/app/src/main/java/ch/holzcloud/holzwave/MainActivity.kt");
const PROJECT_GITIGNORE: &str = include_str!("../gen/android/.gitignore");

/// The permissions the manifest may ask for. A later change that adds one extends this list in the same commit.
const ALLOWED_PERMISSIONS: &[&str] = &["android.permission.INTERNET"];

/// The one ABI and the names it has in the other two places: the Gradle flavour and the Tauri target.
const ABI: &str = "arm64-v8a";

fn json(text: &str) -> Value {
    serde_json::from_str(text).expect("a JSON file")
}

// ---- helpers over text ------------------------------------------------------------------------------------------------

/// The text between the braces that follow the first occurrence of `header`, or `None`.
fn block_after<'a>(text: &'a str, header: &str) -> Option<&'a str> {
    let start = text.find(header)? + header.len();
    let open = start + text[start..].find('{')?;
    let mut depth = 0usize;
    for (offset, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[open + 1..open + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The string literal of the first line that starts with `key` followed by `=` (`key = "value"`), or `None`.
fn string_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(key)?.trim_start().strip_prefix('=')?.trim();
        Some(rest.trim_matches('"').to_owned())
    })
}

/// The integer of the first line `key = <number>`, or `None`.
fn number_value(text: &str, key: &str) -> Option<u32> {
    string_value(text, key)?.parse().ok()
}

/// Whether `word` occurs in `text` with no letter, digit, underscore or hyphen on either side (so `x86` is found in
/// `abiFilters x86` and is not found inside `x86_64` or `not-x86-ish`).
fn contains_word(text: &str, word: &str) -> bool {
    let word_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(word_char) && !after.is_some_and(word_char)
    })
}

// ---- the files of the project ------------------------------------------------------------------------------------------

fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("gen/android")
}

/// The files a commit would hold: the files git tracks plus those it does not ignore, so what a build leaves behind (the
/// ignored `build/` folder, `local.properties`, the generated Tauri settings with their machine paths) is not read. Without
/// git (a source tarball) the working tree is walked and only the folders and names the project's own `.gitignore` lists are
/// skipped.
fn committed_files() -> Vec<PathBuf> {
    let root = project_root();
    let listed = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "-z", "--", "."])
        .current_dir(&root)
        .output();
    if let Ok(output) = listed
        && output.status.success()
        && !output.stdout.is_empty()
    {
        return output
            .stdout
            .split(|b| *b == 0)
            .filter(|name| !name.is_empty())
            .map(|name| root.join(String::from_utf8_lossy(name).as_ref()))
            .filter(|path| path.is_file())
            .collect();
    }
    let mut files = Vec::new();
    walk(&root, &mut files);
    files
}

fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
    const SKIP: &[&str] = &[
        "build", ".gradle", ".cxx", ".externalNativeBuild", ".idea", "captures", ".tauri", "local.properties",
        "tauri.settings.gradle", "tauri.build.gradle.kts", "tauri.properties", "proguard-tauri.pro", "assets", "jniLibs",
        "generated", "schemas",
    ];
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if SKIP.contains(&name.as_str()) {
            continue;
        }
        if path.is_dir() {
            walk(&path, files);
        } else {
            files.push(path);
        }
    }
}

/// The committed files that are text, with their path below the project root.
fn committed_text() -> Vec<(String, String)> {
    let root = project_root();
    committed_files()
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let rel = path.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
            Some((rel, text))
        })
        .collect()
}

// ---- the checks ---------------------------------------------------------------------------------------------------------

/// The application id and the namespace are `identifier`, the `.debug` suffix is on the debug build type only and equals
/// the suffix of the Tauri configuration, and the activity of the manifest is a class of that namespace.
fn identity_problems(
    gradle: &str,
    identifier: &str,
    suffix: &str,
    manifest: &str,
    activity_source: &str,
) -> Vec<String> {
    let mut problems = Vec::new();
    match string_value(gradle, "applicationId") {
        Some(id) if id == identifier => {}
        other => problems.push(format!("the application id is {other:?}, not {identifier}")),
    }
    match string_value(gradle, "namespace") {
        Some(ns) if ns == identifier => {}
        other => problems.push(format!("the namespace is {other:?}, not {identifier}")),
    }
    match block_after(gradle, "getByName(\"debug\")") {
        Some(debug) if string_value(debug, "applicationIdSuffix").as_deref() == Some(suffix) => {}
        _ => problems.push(format!("the debug build type has no applicationIdSuffix \"{suffix}\"")),
    }
    match block_after(gradle, "getByName(\"release\")") {
        Some(release) if !release.contains("applicationIdSuffix") => {}
        _ => problems.push("the release build type has an applicationIdSuffix or is missing".into()),
    }
    if !manifest.contains("android:name=\".MainActivity\"") {
        problems.push("the manifest has no activity named .MainActivity".into());
    }
    if !activity_source.lines().any(|line| line.trim() == format!("package {identifier}")) {
        problems.push(format!("MainActivity.kt is not in the package {identifier}"));
    }
    problems
}

/// The minimum SDK is `want` in the Gradle file and in the Tauri configuration of Android; both must say the same.
fn min_sdk_problems(gradle: &str, android_conf: &Value, want: u32) -> Vec<String> {
    let mut problems = Vec::new();
    match number_value(gradle, "minSdk") {
        Some(found) if found == want => {}
        other => problems.push(format!("minSdk in the Gradle file is {other:?}, not {want}")),
    }
    match android_conf["bundle"]["android"]["minSdkVersion"].as_u64() {
        Some(found) if found == u64::from(want) => {}
        other => problems.push(format!("bundle.android.minSdkVersion in tauri.android.conf.json is {other:?}, not {want}")),
    }
    problems
}

/// The ABI filters of the Gradle file are exactly `arm64-v8a`, and the plugin's three lists (ABIs, flavours, Tauri targets)
/// each hold exactly one entry.
fn abi_filter_problems(gradle: &str, plugin: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let default_config = block_after(gradle, "defaultConfig").unwrap_or("");
    let filters: Vec<&str> = default_config
        .lines()
        .filter(|line| line.contains("abiFilters"))
        .filter_map(|line| line.split('"').nth(1))
        .collect();
    if filters != [ABI] {
        problems.push(format!("defaultConfig.ndk.abiFilters is {filters:?}, not exactly [\"{ABI}\"]"));
    }
    for (name, entry) in [("defaultAbiList", ABI), ("defaultArchList", "arm64"), ("targetsList", "aarch64")] {
        let wanted = format!("val {name} = listOf(\"{entry}\")");
        if !plugin.lines().any(|line| line.trim() == wanted) {
            problems.push(format!("the Rust plugin does not fix {name} to exactly [\"{entry}\"]"));
        }
    }
    problems
}

/// No file names another ABI or target: `x86_64`, `x86`, `armeabi-v7a`, `armv7` or `i686` as a whole word.
fn other_abi_problems(files: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, text) in files {
        for word in ["x86_64", "x86", "armeabi-v7a", "armv7", "i686"] {
            if contains_word(text, word) {
                problems.push(format!("{path} names the ABI or target {word}; the app is arm64-v8a only"));
            }
        }
    }
    problems
}

/// Nothing in the committed project is a machine path, a key, a literal password, a Google service or an installer
/// permission.
fn secret_problems(files: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, text) in files {
        let name = path.rsplit('/').next().unwrap_or(path);
        if name == "local.properties" {
            problems.push(format!("{path} is committed: it holds the machine's SDK path"));
        }
        for extension in [".jks", ".keystore", ".p12", ".pem"] {
            if name.ends_with(extension) {
                problems.push(format!("{path} is a key file"));
            }
        }
        for needle in ["/Users/", "/home/", "C:\\Users"] {
            if text.contains(needle) {
                problems.push(format!("{path} contains the machine path {needle}"));
            }
        }
        let lower = text.to_lowercase();
        for needle in [
            "google-services",
            "firebase",
            "com.google.android.gms",
            "request_install_packages",
            "install_packages",
        ] {
            if lower.contains(needle) {
                problems.push(format!("{path} mentions {needle}"));
            }
        }
        for key in ["storePassword", "keyPassword"] {
            for line in text.lines().filter(|line| line.contains(key)) {
                // A literal value is a quoted string or something that is not a lookup of a property or an environment variable.
                let value = line.split(['=', ':']).nth(1).unwrap_or("").trim();
                let lookup = value.contains("getProperty") || value.contains("getenv") || value.contains("properties[");
                if !value.is_empty() && !lookup {
                    problems.push(format!("{path} sets {key} to a literal value"));
                }
            }
        }
    }
    problems
}

/// The project's own `.gitignore` keeps what must never be committed out of git: the machine path file, the signing
/// property files and every kind of key file.
fn gitignore_problems(gitignore: &str) -> Vec<String> {
    let lines: Vec<&str> = gitignore.lines().map(str::trim).collect();
    ["local.properties", "key.properties", "keystore.properties", "*.jks", "*.keystore", "*.p12", "*.pem", "build", "tauri.settings.gradle"]
        .iter()
        .filter(|entry| !lines.iter().any(|line| line.trim_start_matches('/') == **entry))
        .map(|entry| format!("the .gitignore of the project does not list {entry}"))
        .collect()
}

/// The permissions of the manifest are exactly `allowed`, and only the launcher activity is exported.
fn manifest_problems(manifest: &str, allowed: &[&str]) -> Vec<String> {
    let mut problems = Vec::new();
    let asked: Vec<&str> = manifest
        .lines()
        .filter(|line| line.contains("<uses-permission"))
        .filter_map(|line| line.split("android:name=\"").nth(1)?.split('"').next())
        .collect();
    for permission in &asked {
        if !allowed.contains(permission) {
            problems.push(format!("the manifest asks for {permission}, which is not on the list of this test"));
        }
    }
    for permission in allowed {
        if !asked.contains(permission) {
            problems.push(format!("the manifest no longer asks for {permission}; drop it from the list in the same commit"));
        }
    }
    let exported = manifest.matches("android:exported=\"true\"").count();
    if exported != 1 {
        problems.push(format!("{exported} components are exported, not exactly the launcher activity"));
    }
    problems
}

/// The release build type is neither debuggable nor allowed to use cleartext; the cleartext switch is false by default and
/// true only in the debug build type, and the manifest reads it from there instead of fixing it.
fn build_type_problems(gradle: &str, manifest: &str) -> Vec<String> {
    let mut problems = Vec::new();
    match block_after(gradle, "getByName(\"release\")") {
        Some(release) => {
            if release.contains("isDebuggable = true") || release.contains("debuggable true") {
                problems.push("the release build type is debuggable".into());
            }
            if release.contains("usesCleartextTraffic") {
                problems.push("the release build type sets usesCleartextTraffic".into());
            }
        }
        None => problems.push("the Gradle file has no release build type".into()),
    }
    match block_after(gradle, "defaultConfig") {
        Some(default) if default.contains("manifestPlaceholders[\"usesCleartextTraffic\"] = \"false\"") => {}
        _ => problems.push("usesCleartextTraffic is not false by default".into()),
    }
    match block_after(gradle, "getByName(\"debug\")") {
        Some(debug) if debug.contains("isDebuggable = true") => {}
        _ => problems.push("the debug build type is not debuggable (run-as and the emulator proof need it)".into()),
    }
    if !manifest.contains("android:usesCleartextTraffic=\"${usesCleartextTraffic}\"") {
        problems.push("the manifest does not take usesCleartextTraffic from the build type".into());
    }
    problems
}

fn assert_none(what: &str, problems: Vec<String>) {
    assert!(problems.is_empty(), "{what}:\n  {}", problems.join("\n  "));
}

// ---- the project --------------------------------------------------------------------------------------------------------

#[test]
fn the_project_is_a_gradle_build_with_the_files_of_one() {
    let root = project_root();
    assert!(root.is_dir(), "app/src-tauri/gen/android is not in the repository");
    assert!(root.join("settings.gradle").is_file() || root.join("settings.gradle.kts").is_file(), "no settings.gradle");
    assert!(
        root.join("app/build.gradle.kts").is_file() || root.join("app/build.gradle").is_file(),
        "no app/build.gradle"
    );
    assert!(root.join("app/src/main/AndroidManifest.xml").is_file(), "no AndroidManifest.xml");
}

#[test]
fn the_application_id_is_the_identifier_and_only_the_debug_build_has_a_suffix() {
    let tauri = json(TAURI_CONF);
    let identifier = tauri["identifier"].as_str().expect("an identifier");
    assert_eq!(identifier, "ch.holzcloud.holzwave");
    let suffix = tauri["bundle"]["android"]["debugApplicationIdSuffix"].as_str().expect("a debug suffix");
    assert_eq!(suffix, ".debug");
    assert_none("identity", identity_problems(APP_GRADLE, identifier, suffix, MANIFEST, MAIN_ACTIVITY));
}

#[test]
fn identity_check_fails_on_bad_input() {
    let id = "ch.holzcloud.holzwave";
    assert!(!identity_problems(&APP_GRADLE.replace("applicationId = \"ch.holzcloud.holzwave\"", "applicationId = \"x.y\""), id, ".debug", MANIFEST, MAIN_ACTIVITY).is_empty());
    assert!(!identity_problems(&APP_GRADLE.replace("namespace = ", "namespace = \"a.b\" // "), id, ".debug", MANIFEST, MAIN_ACTIVITY).is_empty());
    assert!(!identity_problems(&APP_GRADLE.replace("applicationIdSuffix = \".debug\"", ""), id, ".debug", MANIFEST, MAIN_ACTIVITY).is_empty());
    assert!(!identity_problems(APP_GRADLE, id, ".dbg", MANIFEST, MAIN_ACTIVITY).is_empty());
    let on_release = APP_GRADLE.replace("getByName(\"release\") {", "getByName(\"release\") {\n applicationIdSuffix = \".debug\"");
    assert!(!identity_problems(&on_release, id, ".debug", MANIFEST, MAIN_ACTIVITY).is_empty());
    assert!(!identity_problems(APP_GRADLE, id, ".debug", &MANIFEST.replace(".MainActivity", ".Other"), MAIN_ACTIVITY).is_empty());
    assert!(!identity_problems(APP_GRADLE, id, ".debug", MANIFEST, "package other.pkg\n").is_empty());
}

#[test]
fn the_minimum_sdk_is_31_in_the_project_and_in_the_configuration() {
    assert_none("minSdk", min_sdk_problems(APP_GRADLE, &json(ANDROID_CONF), 31));
}

#[test]
fn min_sdk_check_fails_on_bad_input() {
    assert!(!min_sdk_problems(&APP_GRADLE.replace("minSdk = 31", "minSdk = 24"), &json(ANDROID_CONF), 31).is_empty());
    assert!(!min_sdk_problems(APP_GRADLE, &json(r#"{"bundle":{"android":{"minSdkVersion":24}}}"#), 31).is_empty());
    assert!(!min_sdk_problems(APP_GRADLE, &json("{}"), 31).is_empty());
}

#[test]
fn the_abi_is_exactly_arm64_v8a_and_no_file_names_another() {
    assert_none("abi filters", abi_filter_problems(APP_GRADLE, RUST_PLUGIN));
    assert_none("other ABIs", other_abi_problems(&committed_text()));
}

#[test]
fn abi_checks_fail_on_bad_input() {
    let two = APP_GRADLE.replace("abiFilters.add(\"arm64-v8a\")", "abiFilters.add(\"arm64-v8a\")\n abiFilters.add(\"x86_64\")");
    assert!(!abi_filter_problems(&two, RUST_PLUGIN).is_empty());
    assert!(!abi_filter_problems(&APP_GRADLE.replace("abiFilters.add(\"arm64-v8a\")", ""), RUST_PLUGIN).is_empty());
    let wide = RUST_PLUGIN.replace("val targetsList = listOf(\"aarch64\")", "val targetsList = listOf(\"aarch64\", \"armv7\")");
    assert!(!abi_filter_problems(APP_GRADLE, &wide).is_empty());
    for bad in ["x86_64", "x86", "armeabi-v7a", "armv7", "i686"] {
        let files = vec![("a.gradle".to_owned(), format!("abiFilters += \"{bad}\""))];
        assert!(!other_abi_problems(&files).is_empty(), "{bad} was not found");
    }
    // A word that merely contains one of them is not a hit, and prose about the arm64 ABI is fine.
    let fine = vec![("a.txt".to_owned(), "arm64-v8a aarch64 x86_64suffix not-x86 xx86".to_owned())];
    assert!(other_abi_problems(&fine).is_empty(), "{:?}", other_abi_problems(&fine));
}

#[test]
fn no_committed_file_holds_a_machine_path_a_key_or_a_service() {
    let files = committed_text();
    assert!(files.len() > 10, "the committed project was not found ({} text files)", files.len());
    assert_none("committed project", secret_problems(&files));
    assert_none("gitignore", gitignore_problems(PROJECT_GITIGNORE));
}

#[test]
fn secret_checks_fail_on_bad_input() {
    let text = |name: &str, body: &str| vec![(name.to_owned(), body.to_owned())];
    assert!(!secret_problems(&text("local.properties", "sdk.dir=x")).is_empty());
    for key in ["release.jks", "a.keystore", "b.p12", "c.pem"] {
        assert!(!secret_problems(&text(key, "")).is_empty(), "{key}");
    }
    for body in ["/Users/me/sdk", "/home/me/sdk", "C:\\Users\\me"] {
        assert!(!secret_problems(&text("x.gradle", body)).is_empty(), "{body}");
    }
    for body in [
        "apply plugin: 'com.google.gms.google-services'",
        "implementation 'com.google.firebase:firebase-core'",
        "com.google.android.gms.maps",
        "<uses-permission android:name=\"android.permission.REQUEST_INSTALL_PACKAGES\" />",
        "<uses-permission android:name=\"android.permission.INSTALL_PACKAGES\" />",
    ] {
        assert!(!secret_problems(&text("m.xml", body)).is_empty(), "{body}");
    }
    assert!(!secret_problems(&text("a.gradle", "storePassword = \"hunter2\"")).is_empty());
    assert!(!secret_problems(&text("a.gradle", "keyPassword 'hunter2'\nsigningConfigs { release { keyPassword = \"x\" } }")).is_empty());
    // A property lookup is not a literal.
    assert!(secret_problems(&text("a.gradle", "storePassword = props.getProperty(\"storePassword\")")).is_empty());
    assert!(!gitignore_problems("build\n").is_empty());
    assert!(gitignore_problems("local.properties\nkey.properties\nkeystore.properties\n*.jks\n*.keystore\n*.p12\n*.pem\nbuild\n/tauri.settings.gradle\n").is_empty());
}

#[test]
fn the_manifest_asks_for_internet_only_and_exports_the_launcher_only() {
    assert_none("manifest", manifest_problems(MANIFEST, ALLOWED_PERMISSIONS));
    // Not a failure: the value of allowBackup, reported so that the plan that sets it can see the current state.
    let backup = MANIFEST.split("android:allowBackup=\"").nth(1).and_then(|rest| rest.split('"').next());
    eprintln!("android_project: allowBackup is {backup:?} (not set means the Android default, true; the release plan sets it)");
}

#[test]
fn manifest_check_fails_on_bad_input() {
    let extra = MANIFEST.replace(
        "<uses-permission android:name=\"android.permission.INTERNET\" />",
        "<uses-permission android:name=\"android.permission.INTERNET\" />\n<uses-permission android:name=\"android.permission.CAMERA\" />",
    );
    assert!(!manifest_problems(&extra, ALLOWED_PERMISSIONS).is_empty());
    assert!(!manifest_problems(&MANIFEST.replace("android.permission.INTERNET", "android.permission.NETWORK"), ALLOWED_PERMISSIONS).is_empty());
    let more_exported = MANIFEST.replace("android:exported=\"false\"", "android:exported=\"true\"");
    assert!(!manifest_problems(&more_exported, ALLOWED_PERMISSIONS).is_empty());
}

#[test]
fn only_the_debug_build_is_debuggable_and_may_use_cleartext() {
    assert_none("build types", build_type_problems(APP_GRADLE, MANIFEST));
}

#[test]
fn build_type_check_fails_on_bad_input() {
    let debuggable_release = APP_GRADLE.replace("getByName(\"release\") {", "getByName(\"release\") {\n isDebuggable = true");
    assert!(!build_type_problems(&debuggable_release, MANIFEST).is_empty());
    let cleartext_release =
        APP_GRADLE.replace("getByName(\"release\") {", "getByName(\"release\") {\n manifestPlaceholders[\"usesCleartextTraffic\"] = \"true\"");
    assert!(!build_type_problems(&cleartext_release, MANIFEST).is_empty());
    let default_true = APP_GRADLE.replace(
        "manifestPlaceholders[\"usesCleartextTraffic\"] = \"false\"",
        "manifestPlaceholders[\"usesCleartextTraffic\"] = \"true\"",
    );
    assert!(!build_type_problems(&default_true, MANIFEST).is_empty());
    assert!(!build_type_problems(&APP_GRADLE.replace("isDebuggable = true", ""), MANIFEST).is_empty());
    assert!(!build_type_problems(APP_GRADLE, &MANIFEST.replace("${usesCleartextTraffic}", "true")).is_empty());
}

#[test]
fn the_block_and_word_helpers_do_what_the_checks_rely_on() {
    assert_eq!(block_after("a { b { c } d } e", "a").map(str::trim), Some("b { c } d"));
    assert_eq!(block_after("a { unclosed", "a"), None);
    assert_eq!(block_after("nothing", "a"), None);
    assert_eq!(string_value("  minSdk = 31\n", "minSdk").as_deref(), Some("31"));
    assert_eq!(number_value("x\n minSdk = 31", "minSdk"), Some(31));
    assert!(contains_word("abiFilters x86", "x86") && !contains_word("x86_64", "x86") && !contains_word("not-x86", "x86"));
}
