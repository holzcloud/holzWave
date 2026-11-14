//! Text tests of what the Android build promises to a store and to a phone: the version code that follows the version of the
//! app, the hardening of the manifest, the one list of permissions and the Gradle settings that keep the release unsigned and
//! free of services. They read files only; no Android tool is needed, so they run on the desktop in `cargo test`.
//!
//! Every check is a small pure function that returns a list of problems, one sentence each, and every function has a test on
//! a bad input, so a check that can never fail is caught here. If a project file contradicts a check, the file is fixed, not
//! the check.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const TAURI_CONF: &str = include_str!("../tauri.conf.json");
const ANDROID_CONF: &str = include_str!("../tauri.android.conf.json");
const VERSION_SCRIPT: &str = include_str!("../../../scripts/android-version-code.sh");

fn json(text: &str) -> Value {
    serde_json::from_str(text).expect("a JSON file")
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

// ---- the version code ---------------------------------------------------------------------------------------------------

/// The scheme of `scripts/android-version-code.sh`, written again so the two can be compared on a table:
/// `major * 10000000 + minor * 100000 + patch * 1000 + stage * 100 + number`, with the stage 1 alpha, 2 beta, 3 rc and 9 final.
/// The largest major is 209, so that the code stays below Android's limit of 2100000000.
fn version_code(version: &str) -> Result<u32, String> {
    if version.is_empty() {
        return Err("the version is empty".into());
    }
    if version.contains('+') {
        return Err(format!("build metadata is not allowed in {version}"));
    }
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let number = |what: &str, text: &str, max: u32| -> Result<u32, String> {
        if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("{what} {text:?} is not a number in {version}"));
        }
        if text.len() > 1 && text.starts_with('0') {
            return Err(format!("{what} {text:?} has a leading zero in {version}"));
        }
        match text.parse::<u32>() {
            Ok(value) if value <= max => Ok(value),
            _ => Err(format!("{what} {text:?} is above {max} in {version}")),
        }
    };
    let parts: Vec<&str> = core.split('.').collect();
    let [major, minor, patch] = parts[..] else {
        return Err(format!("{version} is not major.minor.patch"));
    };
    let (major, minor, patch) = (
        number("major", major, 209)?,
        number("minor", minor, 99)?,
        number("patch", patch, 99)?,
    );
    let (stage, n) = match pre {
        None => (9, 0),
        Some(pre) => {
            let word_end = pre.find(|c: char| c.is_ascii_digit()).unwrap_or(pre.len());
            let (word, digits) = pre.split_at(word_end);
            let stage = match word {
                "alpha" => 1,
                "beta" => 2,
                "rc" => 3,
                _ => {
                    return Err(format!(
                        "the pre-release stage {word:?} of {version} is not alpha, beta or rc"
                    ));
                }
            };
            let n = number("pre-release number", digits, 99)?;
            if n < 1 {
                return Err(format!(
                    "the pre-release number of {version} must be 1 to 99"
                ));
            }
            (stage, n)
        }
    };
    Ok(major * 10_000_000 + minor * 100_000 + patch * 1_000 + stage * 100 + n)
}

/// The table of the scheme: the same rows as `scripts/test-android-version-code.sh`.
const TABLE: &[(&str, u32)] = &[
    ("0.0.1-alpha1", 1101),
    ("0.0.1-alpha2", 1102),
    ("0.0.1-beta1", 1201),
    ("0.0.1-beta2", 1202),
    ("0.0.1-rc1", 1301),
    ("0.0.1", 1900),
    ("0.0.2-alpha1", 2101),
    ("0.1.0", 100_900),
    ("1.0.0", 10_000_900),
    ("0.0.1-alpha99", 1199),
    ("0.0.1-rc99", 1399),
    ("209.99.99", 2_099_999_900),
    ("209.99.99-rc99", 2_099_999_399),
];

/// The `versionCode` of `tauri.android.conf.json` is the scheme applied to the `version` of `tauri.conf.json`.
fn version_code_problems(tauri_conf: &Value, android_conf: &Value) -> Vec<String> {
    let Some(version) = tauri_conf["version"].as_str() else {
        return vec!["tauri.conf.json has no version".into()];
    };
    let wanted = match version_code(version) {
        Ok(code) => code,
        Err(why) => {
            return vec![format!(
                "the version {version} of the app has no version code: {why}"
            )];
        }
    };
    match android_conf["bundle"]["android"]["versionCode"].as_u64() {
        Some(found) if found == u64::from(wanted) => Vec::new(),
        found => vec![format!(
            "bundle.android.versionCode in tauri.android.conf.json is {found:?} but the version {version} of the app has the code \
             {wanted}; run `sh scripts/android-version-code.sh {version}` and write the number into the file in the same commit as \
             the version"
        )],
    }
}

#[test]
fn the_scheme_gives_the_table() {
    for (version, code) in TABLE {
        assert_eq!(version_code(version), Ok(*code), "{version}");
    }
    assert!(version_code("209.99.99").unwrap() < 2_100_000_000);
}

#[test]
fn the_scheme_orders_alpha_beta_rc_and_final_of_every_version() {
    let mut versions = Vec::new();
    for major in [0, 1, 2, 10, 209] {
        for minor in [0, 1, 9, 10, 99] {
            for patch in [0, 1, 2, 10, 99] {
                for pre in [
                    "alpha1", "alpha99", "beta1", "beta2", "beta99", "rc1", "rc99",
                ] {
                    versions.push((major, minor, patch, pre.to_owned()));
                }
                versions.push((major, minor, patch, String::new()));
            }
        }
    }
    let stage_rank = |pre: &str| -> (u32, u32) {
        if pre.is_empty() {
            return (9, 0);
        }
        let at = pre.find(|c: char| c.is_ascii_digit()).unwrap();
        let rank = match &pre[..at] {
            "alpha" => 1,
            "beta" => 2,
            _ => 3,
        };
        (rank, pre[at..].parse().unwrap())
    };
    versions.sort_by_key(|(major, minor, patch, pre)| (*major, *minor, *patch, stage_rank(pre)));
    let codes: Vec<u32> = versions
        .iter()
        .map(|(major, minor, patch, pre)| {
            let text = if pre.is_empty() {
                format!("{major}.{minor}.{patch}")
            } else {
                format!("{major}.{minor}.{patch}-{pre}")
            };
            version_code(&text).unwrap_or_else(|why| panic!("{text}: {why}"))
        })
        .collect();
    assert_eq!(versions.len(), 1000);
    for pair in codes.windows(2) {
        assert!(
            pair[0] < pair[1],
            "the codes are not strictly increasing: {} then {}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn the_scheme_refuses_what_it_cannot_order() {
    for bad in [
        "0.0.100",
        "0.100.0",
        "210.0.0",
        "0.0.1-beta100",
        "0.0.1-beta0",
        "0.0.1-beta",
        "0.0.1-gamma1",
        "0.0.1-beta.2",
        "0.0.1+5",
        "0.0.1-beta2+x",
        "",
        "gamma",
        "1.0",
        "1.0.0.0",
        "0.01.0",
        "0.-1.0",
        "0.0.99999999999999999999",
    ] {
        assert!(version_code(bad).is_err(), "{bad:?} was not refused");
    }
}

#[test]
fn the_script_and_the_test_agree_on_the_table() {
    // The script is run on every row of the table and on the refusals: the one written in the script and the one written in
    // this test must not drift apart.
    let script = repository_root().join("scripts/android-version-code.sh");
    for (version, code) in TABLE {
        let output = Command::new("sh")
            .arg(&script)
            .arg(version)
            .output()
            .expect("sh runs the script");
        assert!(
            output.status.success(),
            "{version}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            code.to_string(),
            "{version}"
        );
    }
    for bad in ["0.0.100", "0.0.1-gamma1", "0.0.1+5", "", "0.0.1-beta0"] {
        let output = Command::new("sh")
            .arg(&script)
            .arg(bad)
            .output()
            .expect("sh runs the script");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{bad:?} was not refused by the script"
        );
        assert!(
            output.stdout.is_empty(),
            "{bad:?}: the script printed on stdout"
        );
    }
}

#[test]
fn the_scheme_is_written_down_once_in_the_header_of_the_script() {
    let header: String = VERSION_SCRIPT
        .lines()
        .take_while(|line| line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for word in [
        "major * 10000000",
        "minor * 100000",
        "patch * 1000",
        "stage * 100",
        "alpha",
        "beta",
        "release candidate",
        "final",
        "209",
        "2100000000",
    ] {
        assert!(
            header.contains(word),
            "the header of the version script does not say {word:?}"
        );
    }
}

#[test]
fn the_version_code_in_the_android_configuration_follows_the_version_of_the_app() {
    let problems = version_code_problems(&json(TAURI_CONF), &json(ANDROID_CONF));
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn version_code_check_fails_on_bad_input() {
    let tauri = json(TAURI_CONF);
    let android = json(ANDROID_CONF);
    // A drifted code, a missing code, a version the scheme refuses, no version at all.
    let drifted = json(r#"{"bundle":{"android":{"versionCode":1}}}"#);
    let problems = version_code_problems(&tauri, &drifted);
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0].contains("android-version-code.sh"),
        "{problems:?}"
    );
    assert!(!version_code_problems(&tauri, &json("{}")).is_empty());
    assert!(!version_code_problems(&json(r#"{"version":"0.0.1-gamma1"}"#), &android).is_empty());
    assert!(!version_code_problems(&json("{}"), &android).is_empty());
    // A bump of the version without the code is the failure this test exists for.
    let bumped = json(r#"{"version":"0.0.1-beta3"}"#);
    assert!(!version_code_problems(&bumped, &android).is_empty());
    assert!(version_code_problems(&json(r#"{"version":"0.0.1-beta2"}"#), &android).is_empty());
    // The code is an integer in the file, not a string.
    assert!(
        !version_code_problems(
            &json(r#"{"version":"0.0.1-beta2"}"#),
            &json(r#"{"bundle":{"android":{"versionCode":"1202"}}}"#)
        )
        .is_empty()
    );
}

// ---- the files of the Android project ------------------------------------------------------------------------------------

const PERMISSION_LIST: &str = include_str!("../android-permissions.txt");
const APP_GRADLE: &str = include_str!("../gen/android/app/build.gradle.kts");
const BUILD_SCRIPT: &str = include_str!("../../../scripts/android-build.sh");

fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("gen/android")
}

/// The files a commit would hold (tracked, or untracked and not ignored), so what a build leaves behind is not read.
/// Without git the working tree is walked and the folders a build writes are skipped.
fn project_files() -> Vec<PathBuf> {
    let root = project_root();
    let listed = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
        ])
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
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) {
        const SKIP: &[&str] = &[
            "build",
            ".gradle",
            ".cxx",
            ".idea",
            "generated",
            "assets",
            "jniLibs",
        ];
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if SKIP.contains(&name.as_str()) {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                walk(&path, files);
            } else {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&root, &mut files);
    files
}

/// The text files of the project with their path below `gen/android`, `/`-separated.
fn project_text() -> Vec<(String, String)> {
    let root = project_root();
    project_files()
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            let rel = path
                .strip_prefix(&root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            Some((rel, text))
        })
        .collect()
}

fn find<'a>(files: &'a [(String, String)], path: &str) -> &'a str {
    files.iter().find(|(name, _)| name == path).map_or_else(
        || panic!("{path} is not in the Android project"),
        |(_, text)| text.as_str(),
    )
}

/// The manifests of the project: every `AndroidManifest.xml` of every source set.
fn manifests(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .filter(|(name, _)| name.rsplit('/').next() == Some("AndroidManifest.xml"))
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect()
}

// ---- a small reader of Android XML --------------------------------------------------------------------------------------

/// One start tag: its name, its attributes as written (`android:exported`, `tools:node`) in document order, and the text up
/// to the next tag (the host name of a `<domain>` element).
#[derive(Debug, PartialEq)]
struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
}

impl Tag {
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

/// Every start tag of an XML text in document order, comments, declarations and end tags left out. Enough for the manifest and
/// the resource files of Android, which have no CDATA and no `>` inside an attribute value.
fn tags(xml: &str) -> Vec<Tag> {
    let mut text = String::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<!--") {
        text.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    text.push_str(rest);
    let mut found = Vec::new();
    for piece in text.split('<').skip(1) {
        let Some((body, after_tag)) = piece.split_once('>') else {
            continue;
        };
        if body.starts_with('/') || body.starts_with('?') || body.starts_with('!') {
            continue;
        }
        let body = body.trim_end_matches('/');
        let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
        let (name, mut attrs_text) = body.split_at(name_end);
        let mut attrs = Vec::new();
        while let Some(eq) = attrs_text.find('=') {
            let key = attrs_text[..eq].trim().to_owned();
            let after = attrs_text[eq + 1..].trim_start();
            let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                break;
            };
            let Some(close) = after[1..].find(quote) else {
                break;
            };
            attrs.push((key, after[1..1 + close].to_owned()));
            attrs_text = &after[close + 2..];
        }
        found.push(Tag {
            name: name.to_owned(),
            attrs,
            text: after_tag.trim().to_owned(),
        });
    }
    found
}

fn assert_none(what: &str, problems: Vec<String>) {
    assert!(problems.is_empty(), "{what}:\n  {}", problems.join("\n  "));
}

#[test]
fn the_xml_reader_does_what_the_checks_rely_on() {
    let xml = "<?xml version=\"1.0\"?>\n<!-- <receiver android:exported=\"true\" /> -->\n<manifest a:b=\"1\">\n <application x:y='two' z=\"3\"/>\n</manifest>";
    let found = tags(xml);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].name, "manifest");
    assert_eq!(found[1].attr("x:y"), Some("two"));
    assert_eq!(found[1].attr("z"), Some("3"));
    assert_eq!(found[1].attr("missing"), None);
    let domain = tags("<domain includeSubdomains=\"false\">10.0.2.2</domain>");
    assert_eq!(domain[0].text, "10.0.2.2");
}

// ---- the permission list (Test 1) ---------------------------------------------------------------------------------------

/// The permissions named by `list`: one per line, comments (`#`) and empty lines left out.
fn listed_permissions(list: &str) -> Vec<&str> {
    list.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// A permission that no change of this app may ask for without a decision of its own.
const FORBIDDEN_PERMISSIONS: &[&str] = &[
    "android.permission.REQUEST_INSTALL_PACKAGES",
    "android.permission.INSTALL_PACKAGES",
    "android.permission.DELETE_PACKAGES",
    "android.permission.SYSTEM_ALERT_WINDOW",
    "android.permission.BIND_ACCESSIBILITY_SERVICE",
    "android.permission.READ_CONTACTS",
    "android.permission.READ_SMS",
    "android.permission.RECEIVE_SMS",
    "android.permission.READ_CALL_LOG",
    "android.permission.ACCESS_FINE_LOCATION",
    "android.permission.ACCESS_BACKGROUND_LOCATION",
    "android.permission.MANAGE_EXTERNAL_STORAGE",
    "android.permission.QUERY_ALL_PACKAGES",
    // The foreground service holds the process and nothing else: no wake lock, no start at boot, no request to leave the
    // battery optimisation.
    "android.permission.WAKE_LOCK",
    "android.permission.RECEIVE_BOOT_COMPLETED",
    "android.permission.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS",
];

/// The manifest's `uses-permission` set equals the list, both ways; the list has no duplicate and none of the forbidden names;
/// and the list says that a later change appends its line in the same commit as the manifest line.
fn permission_problems(manifest: &str, list: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let listed = listed_permissions(list);
    let asked: Vec<String> = tags(manifest)
        .iter()
        .filter(|tag| tag.name == "uses-permission")
        .filter_map(|tag| tag.attr("android:name").map(str::to_owned))
        .collect();
    for permission in &asked {
        if !listed.contains(&permission.as_str()) {
            problems.push(format!(
                "the manifest asks for {permission}, which android-permissions.txt does not list"
            ));
        }
    }
    for permission in &listed {
        if !asked.iter().any(|name| name == permission) {
            problems.push(format!(
                "android-permissions.txt lists {permission}, which the manifest does not ask for"
            ));
        }
        if listed.iter().filter(|other| *other == permission).count() > 1 {
            problems.push(format!(
                "android-permissions.txt lists {permission} more than once"
            ));
        }
        if FORBIDDEN_PERMISSIONS.contains(permission) {
            problems.push(format!("android-permissions.txt lists {permission}, which no change of this app may ask for without its own decision"));
        }
        if permission.contains(char::is_whitespace) || !permission.contains('.') {
            problems.push(format!("{permission:?} is not a permission name"));
        }
    }
    let header: String = list
        .lines()
        .filter(|line| line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    if !header.contains("same commit") || !header.contains("one list") {
        problems.push("android-permissions.txt does not say that it is the one list and that a permission is added in the same commit as the manifest line".into());
    }
    problems
}

#[test]
fn the_manifest_asks_for_exactly_the_permissions_of_the_list() {
    let files = project_text();
    assert_none(
        "permissions",
        permission_problems(
            find(&files, "app/src/main/AndroidManifest.xml"),
            PERMISSION_LIST,
        ),
    );
    assert_eq!(
        listed_permissions(PERMISSION_LIST),
        [
            "android.permission.INTERNET",
            "android.permission.FOREGROUND_SERVICE",
            "android.permission.FOREGROUND_SERVICE_SPECIAL_USE",
            "android.permission.POST_NOTIFICATIONS",
            "android.permission.USE_BIOMETRIC",
            "android.permission.ACCESS_LOCAL_NETWORK",
        ],
        "the list today is the network, the three of the foreground service, the prompt of the secret store and the local network"
    );
}

#[test]
fn permission_check_fails_on_bad_input() {
    let manifest = find(&project_text(), "app/src/main/AndroidManifest.xml").to_owned();
    let extra = manifest.replace(
        "<uses-permission android:name=\"android.permission.INTERNET\" />",
        "<uses-permission android:name=\"android.permission.INTERNET\" />\n<uses-permission android:name=\"android.permission.CAMERA\" />",
    );
    assert!(
        !permission_problems(&extra, PERMISSION_LIST).is_empty(),
        "an extra manifest permission passed"
    );
    let longer = format!("{PERMISSION_LIST}android.permission.CAMERA\n");
    assert!(
        !permission_problems(&manifest, &longer).is_empty(),
        "a listed permission that the manifest lacks passed"
    );
    let installer = format!("{PERMISSION_LIST}android.permission.REQUEST_INSTALL_PACKAGES\n");
    let with_installer = manifest.replace(
        "<uses-permission android:name=\"android.permission.INTERNET\" />",
        "<uses-permission android:name=\"android.permission.INTERNET\" />\n<uses-permission android:name=\"android.permission.REQUEST_INSTALL_PACKAGES\" />",
    );
    assert!(
        !permission_problems(&with_installer, &installer).is_empty(),
        "the install-packages permission passed in both places"
    );
    let twice = format!("{PERMISSION_LIST}android.permission.INTERNET\n");
    assert!(
        !permission_problems(&manifest, &twice).is_empty(),
        "a duplicate passed"
    );
    assert!(
        !permission_problems(&manifest, "android.permission.INTERNET\n").is_empty(),
        "a list without the rule passed"
    );
    assert!(
        !permission_problems(
            &manifest,
            &PERMISSION_LIST.replace("android.permission.INTERNET", "INTERNET now")
        )
        .is_empty()
    );
    // A comment line is not a permission, and a commented-out manifest line is not asked for.
    let commented = manifest.replace(
        "<uses-permission android:name=\"android.permission.INTERNET\" />",
        "<!-- <uses-permission android:name=\"android.permission.INTERNET\" /> -->",
    );
    assert!(!permission_problems(&commented, PERMISSION_LIST).is_empty());
}

// ---- backup and data extraction (Test 2) --------------------------------------------------------------------------------

const EXCLUDED_DOMAINS: &[&str] = &["root", "file", "database", "sharedpref", "external"];

/// The manifest sets `allowBackup` to false, points `dataExtractionRules` at a file of `res/xml` and has no
/// `fullBackupContent`; the rules file excludes whole domains for the cloud backup and for the device transfer and includes
/// nothing.
fn backup_problems(manifest: &str, rules_file: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let mut problems = Vec::new();
    let all = tags(manifest);
    let Some(application) = all.iter().find(|tag| tag.name == "application") else {
        return vec!["the manifest has no application element".into()];
    };
    if application.attr("android:allowBackup") != Some("false") {
        problems.push(format!(
            "android:allowBackup is {:?}, not \"false\"",
            application.attr("android:allowBackup")
        ));
    }
    if application.attr("android:fullBackupContent").is_some() {
        problems.push("android:fullBackupContent is set: the extraction rules are the one place that says what is backed up".into());
    }
    let Some(reference) = application.attr("android:dataExtractionRules") else {
        problems.push("android:dataExtractionRules is not set".into());
        return problems;
    };
    let Some(name) = reference.strip_prefix("@xml/") else {
        problems.push(format!(
            "android:dataExtractionRules is {reference:?}, not a reference of the form @xml/<name>"
        ));
        return problems;
    };
    let Some(rules) = rules_file(name) else {
        problems.push(format!(
            "res/xml/{name}.xml, named by android:dataExtractionRules, does not exist"
        ));
        return problems;
    };
    let rule_tags = tags(&rules);
    for section in ["cloud-backup", "device-transfer"] {
        let Some(at) = rule_tags.iter().position(|tag| tag.name == section) else {
            problems.push(format!("the extraction rules have no <{section}> section"));
            continue;
        };
        let end = rule_tags[at + 1..]
            .iter()
            .position(|tag| tag.name == "cloud-backup" || tag.name == "device-transfer")
            .map_or(rule_tags.len(), |next| at + 1 + next);
        let body = &rule_tags[at + 1..end];
        for domain in EXCLUDED_DOMAINS {
            let excluded = body.iter().any(|tag| {
                tag.name == "exclude"
                    && tag.attr("domain") == Some(domain)
                    && tag.attr("path") == Some(".")
            });
            if !excluded {
                problems.push(format!(
                    "<{section}> does not exclude the whole domain {domain} (path \".\")"
                ));
            }
        }
        if body.iter().any(|tag| tag.name == "include") {
            problems.push(format!(
                "<{section}> includes something: nothing may be copied"
            ));
        }
        if rule_tags[at]
            .attr("disableIfNoEncryptionCapabilities")
            .is_some()
        {
            problems.push(format!("<{section}> relies on disableIfNoEncryptionCapabilities instead of excluding everything"));
        }
    }
    problems
}

fn read_rules(files: &[(String, String)]) -> impl Fn(&str) -> Option<String> + '_ {
    move |name| {
        files
            .iter()
            .find(|(path, _)| *path == format!("app/src/main/res/xml/{name}.xml"))
            .map(|(_, text)| text.clone())
    }
}

#[test]
fn the_app_opts_out_of_backup_and_device_transfer_whole() {
    let files = project_text();
    assert_none(
        "backup",
        backup_problems(
            find(&files, "app/src/main/AndroidManifest.xml"),
            read_rules(&files),
        ),
    );
}

#[test]
fn backup_check_fails_on_bad_input() {
    let files = project_text();
    let manifest = find(&files, "app/src/main/AndroidManifest.xml").to_owned();
    let rules = find(&files, "app/src/main/res/xml/data_extraction_rules.xml").to_owned();
    let with_rules = |rules: String| move |_: &str| Some(rules.clone());
    assert!(backup_problems(&manifest, with_rules(rules.clone())).is_empty());
    assert!(
        !backup_problems(
            &manifest.replace("android:allowBackup=\"false\"", ""),
            with_rules(rules.clone())
        )
        .is_empty(),
        "no allowBackup passed"
    );
    assert!(
        !backup_problems(
            &manifest.replace(
                "android:allowBackup=\"false\"",
                "android:allowBackup=\"true\""
            ),
            with_rules(rules.clone())
        )
        .is_empty()
    );
    assert!(
        !backup_problems(
            &manifest.replace(
                "android:dataExtractionRules=\"@xml/data_extraction_rules\"",
                ""
            ),
            with_rules(rules.clone())
        )
        .is_empty()
    );
    assert!(
        !backup_problems(
            &manifest.replace("@xml/data_extraction_rules", "@xml/other"),
            |_| None
        )
        .is_empty(),
        "a missing rules file passed"
    );
    let old_style = manifest.replace(
        "<application",
        "<application android:fullBackupContent=\"@xml/backup\"",
    );
    assert!(!backup_problems(&old_style, with_rules(rules.clone())).is_empty());
    for domain in EXCLUDED_DOMAINS {
        let missing = rules.replace(&format!("<exclude domain=\"{domain}\" path=\".\" />"), "");
        assert!(
            !backup_problems(&manifest, with_rules(missing)).is_empty(),
            "the domain {domain} was not required"
        );
    }
    // One section without the exclusions is a failure even if the other has them.
    let only_cloud = rules.split("<device-transfer>").next().unwrap().to_owned()
        + "<device-transfer>\n</device-transfer>\n</data-extraction-rules>";
    assert!(!backup_problems(&manifest, with_rules(only_cloud)).is_empty());
    let no_transfer = rules.replace("device-transfer", "device-move");
    assert!(!backup_problems(&manifest, with_rules(no_transfer)).is_empty());
    let includes = rules.replace(
        "<cloud-backup>",
        "<cloud-backup>\n<include domain=\"file\" path=\"vault\" />",
    );
    assert!(!backup_problems(&manifest, with_rules(includes)).is_empty());
    let partial = rules.replace("path=\".\"", "path=\"cache\"");
    assert!(
        !backup_problems(&manifest, with_rules(partial)).is_empty(),
        "a partial exclusion passed"
    );
}

// ---- cleartext traffic (Test 3) -----------------------------------------------------------------------------------------

const DEBUG_SOURCE_SET: &str = "app/src/debug/";

/// Cleartext, and everything that allows it, exists only in the debug source set; there it is allowed to the emulator's alias
/// of the host machine and to nothing else.
fn cleartext_problems(files: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, text) in files {
        let in_debug = path.starts_with(DEBUG_SOURCE_SET);
        let all = tags(text);
        let is_xml = path.ends_with(".xml");
        if is_xml && !in_debug {
            for tag in &all {
                if tag.attr("android:usesCleartextTraffic").is_some() {
                    problems.push(format!(
                        "{path} sets usesCleartextTraffic outside the debug source set"
                    ));
                }
                if tag.attr("android:networkSecurityConfig").is_some() {
                    problems.push(format!(
                        "{path} names a network security config outside the debug source set"
                    ));
                }
                if tag
                    .attr("cleartextTrafficPermitted")
                    .is_some_and(|value| value != "false")
                {
                    problems.push(format!(
                        "{path} permits cleartext outside the debug source set"
                    ));
                }
            }
        }
        if !is_xml && text.contains("usesCleartextTraffic") {
            problems.push(format!("{path} mentions usesCleartextTraffic: cleartext is configured in the debug source set only"));
        }
        if in_debug && is_xml {
            for tag in &all {
                if tag.attr("android:usesCleartextTraffic") == Some("true") {
                    problems.push(format!(
                        "{path} allows cleartext to every address with usesCleartextTraffic"
                    ));
                }
            }
        }
    }
    // The debug source set: a manifest overlay names the config, and the config permits cleartext to 10.0.2.2 only.
    let overlay = files
        .iter()
        .find(|(path, _)| path == "app/src/debug/AndroidManifest.xml");
    let config = files
        .iter()
        .find(|(path, _)| path == "app/src/debug/res/xml/network_security_config.xml");
    match (overlay, config) {
        (Some((_, overlay)), Some((_, config))) => {
            let named = tags(overlay)
                .iter()
                .find(|tag| tag.name == "application")
                .and_then(|tag| tag.attr("android:networkSecurityConfig").map(str::to_owned));
            if named.as_deref() != Some("@xml/network_security_config") {
                problems.push(format!("the debug manifest names {named:?} as its network security config, not @xml/network_security_config"));
            }
            let all = tags(config);
            let mut allowed_domains = Vec::new();
            let mut in_permitting = false;
            let mut permitting_blocks = 0;
            for tag in &all {
                match tag.name.as_str() {
                    "base-config" => {
                        if tag.attr("cleartextTrafficPermitted") != Some("false") {
                            problems.push("the base config of the debug network security config does not refuse cleartext".into());
                        }
                        in_permitting = false;
                    }
                    "domain-config" => {
                        in_permitting = tag.attr("cleartextTrafficPermitted") == Some("true");
                        if in_permitting {
                            permitting_blocks += 1;
                        }
                    }
                    "domain" if in_permitting => {
                        allowed_domains.push((
                            tag.text.clone(),
                            tag.attr("includeSubdomains").unwrap_or("false").to_owned(),
                        ));
                    }
                    _ => {}
                }
            }
            if permitting_blocks != 1 {
                problems.push(format!("the debug network security config has {permitting_blocks} domain configs that permit cleartext, not one"));
            }
            if allowed_domains != [("10.0.2.2".to_owned(), "false".to_owned())] {
                problems.push(format!("cleartext is permitted to {allowed_domains:?}, not to exactly 10.0.2.2 without subdomains"));
            }
        }
        _ => problems.push(
            "the debug source set has no manifest overlay and network security config".into(),
        ),
    }
    problems
}

#[test]
fn cleartext_exists_in_the_debug_source_set_only_and_only_to_the_emulator_host() {
    assert_none("cleartext", cleartext_problems(&project_text()));
}

#[test]
fn cleartext_check_fails_on_bad_input() {
    let files = project_text();
    let with = |path: &str, text: String| -> Vec<(String, String)> {
        files
            .iter()
            .map(|(name, old)| {
                (
                    name.clone(),
                    if name == path {
                        text.clone()
                    } else {
                        old.clone()
                    },
                )
            })
            .collect()
    };
    let main = "app/src/main/AndroidManifest.xml";
    let config = "app/src/debug/res/xml/network_security_config.xml";
    let overlay = "app/src/debug/AndroidManifest.xml";
    assert!(cleartext_problems(&files).is_empty());
    let main_text = find(&files, main).to_owned();
    for attr in [
        "android:usesCleartextTraffic=\"false\"",
        "android:usesCleartextTraffic=\"true\"",
        "android:networkSecurityConfig=\"@xml/network_security_config\"",
    ] {
        let bad = with(
            main,
            main_text.replace("<application", &format!("<application {attr}")),
        );
        assert!(
            !cleartext_problems(&bad).is_empty(),
            "{attr} in the main manifest passed"
        );
    }
    let config_text = find(&files, config).to_owned();
    assert!(
        !cleartext_problems(&with(
            config,
            config_text.replace("10.0.2.2", "192.168.1.5")
        ))
        .is_empty(),
        "another host passed"
    );
    assert!(
        !cleartext_problems(&with(
            config,
            config_text.replace("includeSubdomains=\"false\"", "includeSubdomains=\"true\"")
        ))
        .is_empty()
    );
    assert!(!cleartext_problems(&with(config, config_text.replace("<domain-config cleartextTrafficPermitted=\"true\">", "<domain-config cleartextTrafficPermitted=\"true\"><domain includeSubdomains=\"true\">example.org</domain>"))).is_empty());
    assert!(
        !cleartext_problems(&with(
            config,
            config_text.replace(
                "<base-config cleartextTrafficPermitted=\"false\" />",
                "<base-config cleartextTrafficPermitted=\"true\" />"
            )
        ))
        .is_empty(),
        "a blanket base config passed"
    );
    // No base config at all is fine: the default of this target SDK refuses cleartext.
    assert!(
        cleartext_problems(&with(
            config,
            config_text.replace("<base-config cleartextTrafficPermitted=\"false\" />", "")
        ))
        .is_empty()
    );
    let overlay_text = find(&files, overlay).to_owned();
    assert!(
        !cleartext_problems(&with(
            overlay,
            overlay_text.replace("@xml/network_security_config", "@xml/other")
        ))
        .is_empty()
    );
    assert!(
        !cleartext_problems(&with(
            overlay,
            overlay_text.replace(
                "<application",
                "<application android:usesCleartextTraffic=\"true\""
            )
        ))
        .is_empty(),
        "a blanket allow in the debug overlay passed"
    );
    // A release-side config file (any other source set) is refused.
    let mut moved = files.clone();
    moved.push(("app/src/release/res/xml/network_security_config.xml".into(), "<network-security-config><base-config cleartextTrafficPermitted=\"true\" /></network-security-config>".into()));
    assert!(!cleartext_problems(&moved).is_empty());
    // Without the debug files the check says so instead of passing.
    let none: Vec<_> = files
        .iter()
        .filter(|(name, _)| !name.starts_with(DEBUG_SOURCE_SET))
        .cloned()
        .collect();
    assert!(!cleartext_problems(&none).is_empty());
    // A Gradle placeholder for cleartext is refused too.
    let mut gradle = files.clone();
    gradle.push((
        "app/extra.gradle.kts".into(),
        "manifestPlaceholders[\"usesCleartextTraffic\"] = \"true\"".into(),
    ));
    assert!(!cleartext_problems(&gradle).is_empty());
}

// ---- exported components (Test 4) ---------------------------------------------------------------------------------------

const COMPONENTS: &[&str] = &[
    "activity",
    "activity-alias",
    "service",
    "receiver",
    "provider",
];

/// In every manifest of the project each component says whether it is exported, and the only one that is, is the activity
/// with the launcher intent filter. A component that a manifest removes from the merge (`tools:node="remove"`) still has to say
/// `false`, so that a removal marker cannot be mistaken for an export. Components of the libraries are not in these files; the
/// APK check reads the merged manifest of a build.
fn export_problems(manifests: &[(&str, &str)]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut exported = 0;
    for (path, text) in manifests {
        let all = tags(text);
        for (at, tag) in all.iter().enumerate() {
            if !COMPONENTS.contains(&tag.name.as_str()) {
                continue;
            }
            let name = tag.attr("android:name").unwrap_or("?");
            match tag.attr("android:exported") {
                Some("false") => {}
                Some("true") => {
                    exported += 1;
                    // The filter that follows belongs to this component until the next component starts.
                    let end = all[at + 1..]
                        .iter()
                        .position(|next| COMPONENTS.contains(&next.name.as_str()))
                        .map_or(all.len(), |next| at + 1 + next);
                    let body = &all[at + 1..end];
                    let main = body.iter().any(|t| {
                        t.name == "action"
                            && t.attr("android:name") == Some("android.intent.action.MAIN")
                    });
                    let launcher = body.iter().any(|t| {
                        t.name == "category"
                            && t.attr("android:name") == Some("android.intent.category.LAUNCHER")
                    });
                    if tag.name != "activity" || name != ".MainActivity" || !main || !launcher {
                        problems.push(format!(
                            "{path}: the {} {name} is exported and is not the launcher activity",
                            tag.name
                        ));
                    }
                }
                other => problems.push(format!(
                    "{path}: the {} {name} has android:exported {other:?}: it must say \"false\"",
                    tag.name
                )),
            }
        }
    }
    if exported != 1 {
        problems.push(format!(
            "{exported} components are exported, not exactly the launcher activity"
        ));
    }
    problems
}

#[test]
fn only_the_launcher_activity_is_exported_in_every_manifest_of_the_project() {
    let files = project_text();
    let all = manifests(&files);
    assert!(
        all.len() >= 2,
        "the main and the debug manifest were not found ({} manifests)",
        all.len()
    );
    assert_none("exported components", export_problems(&all));
}

#[test]
fn export_check_fails_on_bad_input() {
    let files = project_text();
    let main = find(&files, "app/src/main/AndroidManifest.xml").to_owned();
    let debug = find(&files, "app/src/debug/AndroidManifest.xml").to_owned();
    assert!(export_problems(&[("main", &main), ("debug", &debug)]).is_empty());
    // A service nobody asked for, exported; one that does not say; a second activity with the launcher filter; the launcher
    // activity itself no longer exported (then nothing is exported, which is also wrong).
    let service = |attrs: &str| {
        main.replace(
            "</application>",
            &format!("<service android:name=\".Sync\" {attrs} /></application>"),
        )
    };
    assert!(!export_problems(&[("main", &service("android:exported=\"true\""))]).is_empty());
    assert!(!export_problems(&[("main", &service(""))]).is_empty());
    assert!(export_problems(&[("main", &service("android:exported=\"false\""))]).is_empty());
    let second = main.replace("</application>", "<activity android:name=\".Other\" android:exported=\"true\"><intent-filter><action android:name=\"android.intent.action.MAIN\" /><category android:name=\"android.intent.category.LAUNCHER\" /></intent-filter></activity></application>");
    assert!(!export_problems(&[("main", &second)]).is_empty());
    assert!(
        !export_problems(&[(
            "main",
            &main.replace("android:exported=\"true\"", "android:exported=\"false\"")
        )])
        .is_empty()
    );
    let no_filter = main.replace(
        "android.intent.category.LAUNCHER",
        "android.intent.category.DEFAULT",
    );
    assert!(!export_problems(&[("main", &no_filter)]).is_empty());
    // A component in another manifest (a debug overlay, a later source set) is covered too.
    let overlay = debug.replace(
        "</manifest>",
        "<receiver android:name=\".Boot\" android:exported=\"true\" /></manifest>",
    );
    assert!(!export_problems(&[("main", &main), ("debug", &overlay)]).is_empty());
    let provider = main.replace(
        "</application>",
        "<provider android:name=\".P\" android:authorities=\"x\" /></application>",
    );
    assert!(!export_problems(&[("main", &provider)]).is_empty());
    // A comment is not a component.
    let commented = main.replace(
        "</application>",
        "<!-- <service android:name=\".S\" android:exported=\"true\" /> --></application>",
    );
    assert!(export_problems(&[("main", &commented)]).is_empty());
}

// ---- the Gradle project (Test 5) ----------------------------------------------------------------------------------------

/// The text between the braces after `header`, for a block of the Kotlin or Groovy build files.
fn block<'a>(text: &'a str, header: &str) -> Option<&'a str> {
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

/// The code of a build file: comment lines (`//`, `#`) and the tails after `//` removed.
fn code_of(text: &str) -> String {
    text.lines()
        .map(|line| line.split("//").next().unwrap_or(""))
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Names that a Gradle file of this project may not carry: services of Google, trackers, the Play core library.
const SERVICE_WORDS: &[&str] = &[
    "google-services",
    "com.google.gms",
    "com.google.android.gms",
    "firebase",
    "crashlytics",
    "analytics",
    "sentry",
    "mixpanel",
    "com.google.android.play",
    "play-core",
    "play:core",
    "app-update",
    "appcenter",
];

/// The release variant stays unsigned, is not debuggable, keeps R8 on as the template has it, and leaves the commit out of the
/// APK; the dependency block of Google Play is left out; the ABI filter is the one ABI; and no build file names a Google
/// service, a tracker or the Play core library or holds a signing configuration.
fn gradle_problems(app_gradle: &str, build_files: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    let code = code_of(app_gradle);
    match block(&code, "getByName(\"release\")") {
        Some(release) => {
            if release.contains("signingConfig") {
                problems.push(
                    "the release build type names a signing config: the release is built unsigned"
                        .into(),
                );
            }
            if release.contains("isDebuggable = true") || release.contains("debuggable true") {
                problems.push("the release build type is debuggable".into());
            }
            if !release.contains("optimization") || !release.contains("enable = true") {
                problems.push("the release build type no longer has optimization { enable = true } of the template: that is a decision".into());
            }
            match block(release, "vcsInfo") {
                Some(vcs) if vcs.contains("include = false") && !vcs.contains("include = true") => {
                }
                _ => problems
                    .push("the release build type does not set vcsInfo { include = false }".into()),
            }
        }
        None => problems.push("the Gradle file has no release build type".into()),
    }
    if code.contains("signingConfigs") {
        problems.push("the Gradle file defines signing configs: signing is the owner's step, outside the repository".into());
    }
    if code.contains("vcsInfo") && code.contains("include = true") {
        problems.push("a vcsInfo include is true".into());
    }
    match block(&code, "dependenciesInfo") {
        Some(info)
            if info.contains("includeInApk = false")
                && info.contains("includeInBundle = false") => {}
        _ => problems
            .push("dependenciesInfo does not set includeInApk and includeInBundle to false".into()),
    }
    if !code.contains("abiFilters.add(\"arm64-v8a\")") {
        problems.push("the arm64-v8a ABI filter of the project is missing".into());
    }
    for (path, text) in build_files {
        let lower = code_of(text).to_lowercase();
        for word in SERVICE_WORDS {
            if lower.contains(word) {
                problems.push(format!("{path} names {word}: no Google service, tracker or Play library belongs in this app"));
            }
        }
        if path.ends_with("gradle.properties") && lower.contains("signing") {
            problems.push(format!("{path} configures signing"));
        }
    }
    problems
}

/// The ProGuard and R8 rule files of the project may not use a rule that takes code or values away on purpose
/// (`-assumenosideeffects`, `-assumevalues`), which could remove what the native side calls by name.
fn rule_file_problems(files: &[(String, String)]) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, text) in files.iter().filter(|(path, _)| path.ends_with(".pro")) {
        for line in text
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
        {
            if line.starts_with("-assume")
                || line.starts_with("-dontkeep")
                || line.starts_with("-checkdiscard")
            {
                problems.push(format!(
                    "{path} has the rule {line}, which removes code on purpose"
                ));
            }
        }
    }
    problems
}

fn gradle_build_files(files: &[(String, String)]) -> Vec<(String, String)> {
    files
        .iter()
        .filter(|(path, _)| {
            path.ends_with(".gradle")
                || path.ends_with(".gradle.kts")
                || path.ends_with("gradle.properties")
                || (path.starts_with("buildSrc/") && path.ends_with(".kt"))
        })
        .cloned()
        .collect()
}

#[test]
fn the_gradle_project_is_unsigned_arm64_only_and_free_of_services() {
    let files = project_text();
    let build_files = gradle_build_files(&files);
    assert!(
        build_files.len() >= 4,
        "the Gradle files were not found ({})",
        build_files.len()
    );
    assert_none("gradle", gradle_problems(APP_GRADLE, &build_files));
    assert_none("rule files", rule_file_problems(&files));
}

#[test]
fn gradle_check_fails_on_bad_input() {
    let files = project_text();
    let build_files = gradle_build_files(&files);
    assert!(gradle_problems(APP_GRADLE, &build_files).is_empty());
    let release = |extra: &str| {
        APP_GRADLE.replace(
            "getByName(\"release\") {",
            &format!("getByName(\"release\") {{\n{extra}"),
        )
    };
    for (what, bad) in [
        (
            "a signing config on release",
            release("signingConfig = signingConfigs.getByName(\"debug\")"),
        ),
        ("a debuggable release", release("isDebuggable = true")),
        (
            "a signing configs block",
            APP_GRADLE.replace(
                "android {",
                "android {\n signingConfigs { create(\"r\") { } }",
            ),
        ),
        (
            "vcsInfo on",
            APP_GRADLE.replace("include = false", "include = true"),
        ),
        (
            "dependenciesInfo in the APK",
            APP_GRADLE.replace("includeInApk = false", "includeInApk = true"),
        ),
        (
            "dependenciesInfo in the bundle",
            APP_GRADLE.replace("includeInBundle = false", "includeInBundle = true"),
        ),
        (
            "no dependenciesInfo",
            APP_GRADLE.replace("dependenciesInfo", "somethingElse"),
        ),
        ("no vcsInfo setting", APP_GRADLE.replace("vcsInfo", "other")),
        (
            "optimization off",
            APP_GRADLE.replace("enable = true", "enable = false"),
        ),
        (
            "no ABI filter",
            APP_GRADLE.replace("abiFilters.add(\"arm64-v8a\")", ""),
        ),
    ] {
        assert!(
            !gradle_problems(&bad, &build_files).is_empty(),
            "{what} passed"
        );
    }
    // A service, a tracker or the Play core library in any build file, even in a dependency line.
    for dependency in [
        "implementation(\"com.google.firebase:firebase-analytics:1.0\")",
        "id(\"com.google.gms.google-services\")",
        "implementation(\"com.google.android.play:core:1.10.3\")",
        "implementation(\"com.google.android.gms:play-services-base:18\")",
        "implementation(\"io.sentry:sentry-android:7\")",
        "implementation(\"com.google.firebase:firebase-crashlytics\")",
    ] {
        let mut more = build_files.clone();
        more.push(("app/other.gradle.kts".into(), dependency.into()));
        assert!(
            !gradle_problems(APP_GRADLE, &more).is_empty(),
            "{dependency} passed"
        );
    }
    // Google's Maven repository and the Material library are not services.
    let mut fine = build_files.clone();
    fine.push((
        "x.gradle.kts".into(),
        "google()\nimplementation(\"com.google.android.material:material:1.12.0\")".into(),
    ));
    assert!(gradle_problems(APP_GRADLE, &fine).is_empty());
    // A word in a comment is not a dependency.
    fine.push(("y.gradle.kts".into(), "// no firebase here\n".into()));
    assert!(gradle_problems(APP_GRADLE, &fine).is_empty());
    let rules = vec![(
        "app/x.pro".to_owned(),
        "-assumenosideeffects class a.B { *; }".to_owned(),
    )];
    assert!(!rule_file_problems(&rules).is_empty());
    assert!(
        rule_file_problems(&[(
            "app/x.pro".to_owned(),
            "# -assumenosideeffects\n-keep class a.B".to_owned()
        )])
        .is_empty()
    );
}

// ---- the build script (Test 6, text part; the run with a stand-in command line is in scripts/test-android-scripts.sh) ------

/// The build script sets the reproducibility environment for every build, passes `--locked`, prints the NDK and the Rust version,
/// names the one target, and never signs.
fn build_script_problems(script: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let code: String = script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for needle in [
        "SOURCE_DATE_EPOCH",
        "git log -1 --format=%ct",
        "CARGO_ENCODED_RUSTFLAGS",
        "rustc --version",
        "$ANDROID_NDK_HOME",
    ] {
        if !code.contains(needle) {
            problems.push(format!("the build script does not use {needle}"));
        }
    }
    for from in ["$repo_dir=", "$cargo_home=", "$ANDROID_NDK_HOME="] {
        if !code.contains(&format!("--remap-path-prefix={from}")) {
            problems.push(format!(
                "the build script has no --remap-path-prefix for {from}"
            ));
        }
    }
    if code.matches("-- --locked").count() != 2 {
        problems.push(
            "the build script does not pass --locked on both the debug and the release command"
                .into(),
        );
    }
    if code.matches("--target aarch64").count() != 2 || code.matches("--target ").count() != 2 {
        problems.push(
            "the build script does not pass exactly --target aarch64 on both commands".into(),
        );
    }
    for word in ["x86_64", "armv7", "i686", "--split-per-abi", "--aab"] {
        if code.contains(word) {
            problems.push(format!("the build script names {word}"));
        }
    }
    for word in [
        "keytool",
        "jarsigner",
        "apksigner",
        "keystore",
        "storePassword",
        "keyPassword",
        "--sign",
    ] {
        if code
            .lines()
            .filter(|line| !line.trim_start().starts_with("echo "))
            .any(|line| line.contains(word))
        {
            problems.push(format!("the build script uses {word}: it never signs"));
        }
    }
    if code.contains("/Users/") || code.contains("/home/") {
        problems.push("the build script holds a literal machine path".into());
    }
    problems
}

#[test]
fn the_build_script_sets_the_reproducibility_environment_and_never_signs() {
    assert_none("build script", build_script_problems(BUILD_SCRIPT));
}

#[test]
fn build_script_check_fails_on_bad_input() {
    assert!(build_script_problems(BUILD_SCRIPT).is_empty());
    for (what, bad) in [
        (
            "no source date",
            BUILD_SCRIPT.replace("SOURCE_DATE_EPOCH", "SOMETHING"),
        ),
        (
            "no flags",
            BUILD_SCRIPT.replace("CARGO_ENCODED_RUSTFLAGS", "SOMETHING_ELSE"),
        ),
        (
            "no repository remap",
            BUILD_SCRIPT.replace("--remap-path-prefix=$repo_dir=", "--other=$repo_dir="),
        ),
        (
            "no cargo home remap",
            BUILD_SCRIPT.replace("--remap-path-prefix=$cargo_home=", "--other=$cargo_home="),
        ),
        (
            "no NDK remap",
            BUILD_SCRIPT.replace(
                "--remap-path-prefix=$ANDROID_NDK_HOME=",
                "--other=$ANDROID_NDK_HOME=",
            ),
        ),
        ("no --locked", BUILD_SCRIPT.replace("-- --locked", "")),
        (
            "a second target",
            BUILD_SCRIPT.replace(
                "--target aarch64 --apk --debug",
                "--target aarch64 --target x86_64 --apk --debug",
            ),
        ),
        (
            "signing",
            format!("{BUILD_SCRIPT}\napksigner sign app.apk\n"),
        ),
        ("a keystore", format!("{BUILD_SCRIPT}\nstorePassword=x\n")),
        (
            "a machine path",
            format!("{BUILD_SCRIPT}\ncd /Users/me/repo\n"),
        ),
    ] {
        assert!(!build_script_problems(&bad).is_empty(), "{what} passed");
    }
}

// ---- the foreground service (shells and connections kept alive in the background) ----------------------------------------

const PLUGIN_RS: &str = include_str!("../src/android/plugin.rs");
const SERVICE_KT: &str =
    include_str!("../gen/android/app/src/main/java/ch/holzcloud/holzwave/HolzwaveService.kt");
const PLUGIN_KT: &str =
    include_str!("../gen/android/app/src/main/java/ch/holzcloud/holzwave/HolzwavePlugin.kt");
const APP_RULES: &str = include_str!("../gen/android/app/proguard-rules.pro");

/// The code of a Kotlin or Rust file: the text of comment lines (`//`, `*`, `/*`) left out.
fn source_code(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let line = line.trim_start();
            !(line.starts_with("//") || line.starts_with('*') || line.starts_with("/*"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the manifest, the sources and the rules must say about the service, one sentence per problem. The service is the one
/// component besides the launcher activity; it is not exported, has the type `specialUse` with the public sentence of what it
/// is for, and nothing in the project starts it from a receiver, holds a wake lock, or puts anything but two counts into its
/// notification. The page cannot call the plugin that drives it.
fn service_problems(
    manifest: &str,
    service_kt: &str,
    plugin_kt: &str,
    plugin_rs: &str,
    rules: &str,
) -> Vec<String> {
    let mut problems = Vec::new();
    let all = tags(manifest);
    let services: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, tag)| tag.name == "service")
        .map(|(at, _)| at)
        .collect();
    if services.len() != 1 {
        problems.push(format!(
            "the manifest has {} services, wanted exactly the foreground service",
            services.len()
        ));
    }
    if let Some(&at) = services.first() {
        let tag = &all[at];
        if tag.attr("android:name") != Some(".HolzwaveService") {
            problems.push("the service is not .HolzwaveService".into());
        }
        if tag.attr("android:exported") != Some("false") {
            problems.push("the service does not say android:exported=\"false\"".into());
        }
        if tag.attr("android:foregroundServiceType") != Some("specialUse") {
            problems.push("the service is not of the type specialUse".into());
        }
        let property = all[at + 1..]
            .iter()
            .take_while(|next| !COMPONENTS.contains(&next.name.as_str()))
            .find(|next| next.name == "property");
        match property.and_then(|p| Some((p.attr("android:name")?, p.attr("android:value")?))) {
            Some(("android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE", sentence))
                if sentence.split_whitespace().count() >= 5 && !sentence[..sentence.len() - 1].contains('.') => {}
            _ => problems.push(
                "the service has no PROPERTY_SPECIAL_USE_FGS_SUBTYPE property with one plain sentence".into(),
            ),
        }
    }
    if all
        .iter()
        .any(|tag| tag.name == "receiver" && tag.attr("tools:node") != Some("remove"))
    {
        problems.push("the manifest has a receiver: nothing may start the service from one".into());
    }

    let service = source_code(service_kt);
    let plugin = source_code(plugin_kt);
    for needed in [
        "const val TITLE = \"holzWave is running\"",
        "const val CHANNEL = \"Running sessions\"",
        "const val STOP_ALL = \"Stop all\"",
        "noun(shells, \"shell\", \"shells\")",
        "noun(connections, \"connection\", \"connections\")",
        "if (count == 1) \"1 $one\" else \"$count $many\"",
        "joinToString(\", \")",
        "NotificationManager.IMPORTANCE_LOW",
        "setSound(null, null)",
        "enableVibration(false)",
        "setShowBadge(false)",
        "VISIBILITY_PRIVATE",
        "FOREGROUND_SERVICE_TYPE_SPECIAL_USE",
        "Intent(this, HolzwaveService::class.java).setAction(ACTION_STOP_ALL)",
        "PendingIntent.FLAG_IMMUTABLE",
        ".setOngoing(true)",
    ] {
        if !service.contains(needed) {
            problems.push(format!("HolzwaveService.kt does not say {needed}"));
        }
    }
    // The text of the notification is the counts, from one function, and nothing else is put into it.
    let texts: Vec<&str> = service
        .lines()
        .filter(|line| {
            line.contains("setContentText(")
                || line.contains("setSubText(")
                || line.contains("setTicker(")
        })
        .collect();
    if texts.len() != 1 || !texts[0].contains("setContentText(SessionText.of(shells, connections))")
    {
        problems.push(
            "the notification's text is not SessionText.of(shells, connections) alone".into(),
        );
    }
    for banned in [
        "BigTextStyle",
        "InboxStyle",
        "setStyle(",
        "setContentInfo(",
        "setSubText(",
        "setTicker(",
        "FLAG_MUTABLE",
    ] {
        if service.contains(banned) {
            problems.push(format!("HolzwaveService.kt uses {banned}"));
        }
    }
    for banned in [
        "WakeLock",
        "PowerManager",
        "BOOT_COMPLETED",
        "BroadcastReceiver",
        "IGNORE_BATTERY",
        "ProcessLifecycleOwner",
    ] {
        for (name, code) in [
            ("HolzwaveService.kt", &service),
            ("HolzwavePlugin.kt", &plugin),
        ] {
            if code.contains(banned) {
                problems.push(format!("{name} uses {banned}"));
            }
        }
    }
    if service.contains("startForegroundService(") || !plugin.contains("startForegroundService(") {
        problems.push("the service is started by the plugin and by nothing else".into());
    }
    if !plugin.contains("catch (e: ForegroundServiceStartNotAllowedException)") {
        problems.push("a refused start is not caught".into());
    }
    if !plugin.contains("Lifecycle.State.STARTED") {
        problems.push(
            "the plugin does not check that the app is visible before it starts the service".into(),
        );
    }
    if !plugin.contains("notifications_asked") {
        problems.push("the notification permission is not asked once".into());
    }

    // The page cannot reach the plugin: it registers no handler and no command on the Rust side.
    let rust = source_code(plugin_rs);
    for banned in [
        "invoke_handler",
        "generate_handler",
        "tauri::command",
        "#[command",
    ] {
        if rust.contains(banned) {
            problems.push(format!(
                "plugin.rs has {banned}: the page could call the plugin"
            ));
        }
    }
    if !rust.contains("register_android_plugin(PACKAGE, CLASS)") {
        problems.push("plugin.rs does not register the Kotlin class".into());
    }

    // R8 is on in the release and the Rust side finds the class by name.
    let rules = rules
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    if !rules.contains("-keep class ch.holzcloud.holzwave.HolzwavePlugin")
        || !rules.contains("@app.tauri.annotation.Command")
        || !rules.contains("RuntimeVisibleAnnotations")
    {
        problems.push(
            "the release rules do not keep HolzwavePlugin, its commands and its annotations".into(),
        );
    }
    problems
}

#[test]
fn the_service_is_not_exported_is_of_the_type_special_use_and_says_what_it_is_for() {
    let files = project_text();
    assert_none(
        "the foreground service",
        service_problems(
            find(&files, "app/src/main/AndroidManifest.xml"),
            SERVICE_KT,
            PLUGIN_KT,
            PLUGIN_RS,
            APP_RULES,
        ),
    );
}

#[test]
fn the_service_check_fails_on_bad_input() {
    let files = project_text();
    let manifest = find(&files, "app/src/main/AndroidManifest.xml").to_owned();
    let ok = |manifest: &str, service: &str, plugin: &str, rust: &str, rules: &str| {
        service_problems(manifest, service, plugin, rust, rules)
    };
    assert!(ok(&manifest, SERVICE_KT, PLUGIN_KT, PLUGIN_RS, APP_RULES).is_empty());
    let manifest_cases = [
        (
            "exported",
            manifest.replace(
                "android:name=\".HolzwaveService\"\n            android:exported=\"false\"",
                "android:name=\".HolzwaveService\"\n            android:exported=\"true\"",
            ),
        ),
        (
            "another type",
            manifest.replace(
                "foregroundServiceType=\"specialUse\"",
                "foregroundServiceType=\"dataSync\"",
            ),
        ),
        (
            "no property",
            manifest.replace(
                "android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE",
                "android.app.OTHER",
            ),
        ),
        (
            "two sentences",
            manifest.replace("background\" />", "background. And more.\" />"),
        ),
        (
            "a receiver",
            manifest.replace(
                "</application>",
                "<receiver android:name=\".Boot\" android:exported=\"false\" /></application>",
            ),
        ),
        (
            "a second service",
            manifest.replace(
                "</application>",
                "<service android:name=\".Other\" android:exported=\"false\" /></application>",
            ),
        ),
    ];
    for (what, bad) in manifest_cases {
        assert!(
            !ok(&bad, SERVICE_KT, PLUGIN_KT, PLUGIN_RS, APP_RULES).is_empty(),
            "{what} passed"
        );
    }
    let source_cases = [
        (
            "a host name in the text",
            SERVICE_KT.replace(
                "setContentText(SessionText.of(shells, connections))",
                "setContentText(\"ssh \" + host)",
            ),
        ),
        (
            "a second text",
            SERVICE_KT.replace(".setOngoing(true)", ".setOngoing(true).setSubText(name)"),
        ),
        ("a wake lock", format!("{SERVICE_KT}\nval l = WakeLock()\n")),
        (
            "a mutable intent",
            SERVICE_KT.replace("FLAG_IMMUTABLE", "FLAG_MUTABLE"),
        ),
        (
            "another plural",
            SERVICE_KT.replace("\"connections\")", "\"links\")"),
        ),
        (
            "a loud channel",
            SERVICE_KT.replace("IMPORTANCE_LOW", "IMPORTANCE_DEFAULT"),
        ),
        (
            "a sound",
            SERVICE_KT.replace("setSound(null, null)", "setSound(uri, null)"),
        ),
        (
            "a badge",
            SERVICE_KT.replace("setShowBadge(false)", "setShowBadge(true)"),
        ),
        (
            "an implicit stop",
            SERVICE_KT.replace(
                "Intent(this, HolzwaveService::class.java).setAction",
                "Intent().setAction",
            ),
        ),
    ];
    for (what, bad) in source_cases {
        assert!(
            !ok(&manifest, &bad, PLUGIN_KT, PLUGIN_RS, APP_RULES).is_empty(),
            "{what} passed"
        );
    }
    let plugin_cases = [
        (
            "no visibility check",
            PLUGIN_KT.replace("Lifecycle.State.STARTED", "Lifecycle.State.CREATED"),
        ),
        (
            "a refusal that is not caught",
            PLUGIN_KT.replace(
                "catch (e: ForegroundServiceStartNotAllowedException)",
                "catch (e: Exception)",
            ),
        ),
        (
            "a receiver",
            format!("{PLUGIN_KT}\nclass Boot : BroadcastReceiver()\n"),
        ),
    ];
    for (what, bad) in plugin_cases {
        assert!(
            !ok(&manifest, SERVICE_KT, &bad, PLUGIN_RS, APP_RULES).is_empty(),
            "{what} passed"
        );
    }
    let rust = format!("{PLUGIN_RS}\n.invoke_handler(tauri::generate_handler![a])\n");
    assert!(
        !ok(&manifest, SERVICE_KT, PLUGIN_KT, &rust, APP_RULES).is_empty(),
        "a page-callable handler passed"
    );
    let rules = APP_RULES.replace("-keep class ch.holzcloud.holzwave.HolzwavePlugin", "# gone");
    assert!(
        !ok(&manifest, SERVICE_KT, PLUGIN_KT, PLUGIN_RS, &rules).is_empty(),
        "no keep rule passed"
    );
}

#[test]
fn the_permission_file_and_the_service_agree_that_no_wake_lock_boot_start_or_battery_exemption_exists()
 {
    let listed = listed_permissions(PERMISSION_LIST);
    for forbidden in [
        "android.permission.WAKE_LOCK",
        "android.permission.RECEIVE_BOOT_COMPLETED",
        "android.permission.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS",
    ] {
        assert!(!listed.contains(&forbidden), "{forbidden} is listed");
        assert!(
            FORBIDDEN_PERMISSIONS.contains(&forbidden),
            "{forbidden} is not on the forbidden list"
        );
    }
    for needed in [
        "android.permission.FOREGROUND_SERVICE",
        "android.permission.FOREGROUND_SERVICE_SPECIAL_USE",
        "android.permission.POST_NOTIFICATIONS",
    ] {
        assert!(listed.contains(&needed), "{needed} is not listed");
    }
}

// ---- the secret store: the Keystore, the prompt, the app switcher picture and the debug-only self-test ---------------------

const KEYSTORE_KT: &str =
    include_str!("../gen/android/app/src/main/java/ch/holzcloud/holzwave/KeystoreBridge.kt");
const ANDROID_MOD_RS: &str = include_str!("../src/android/mod.rs");

/// The name of the intent extra that starts a step of the self-test. It may exist in the debug source set and nowhere else.
const SELFTEST_TRIGGER: &str = "holzwave_selftest";

/// The Kotlin of the project itself, every source set: path below `gen/android/app/src` and the text.
fn own_kotlin(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .filter(|(path, _)| {
            path.starts_with("app/src/")
                && path.contains("/java/ch/holzcloud/holzwave/")
                && path.ends_with(".kt")
        })
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect()
}

/// What the project must say about the secret store, one sentence per problem. The key is an AES-256 GCM key of the Android
/// Keystore that needs the person for every use, through the framework prompt with the cipher as its crypto object and with no
/// library; the app switcher picture is hidden by the framework call and by nothing else; the window is never made secure,
/// so screenshots stay possible; and the self-test and the name of its trigger exist in the debug source set only.
fn secret_store_problems(
    files: &[(String, String)],
    keystore_kt: &str,
    plugin_kt: &str,
    mod_rs: &str,
    gradle: &[(String, String)],
) -> Vec<String> {
    let mut problems = Vec::new();
    let kotlin = own_kotlin(files);
    if kotlin.is_empty() {
        problems.push("no Kotlin source of the project was found".into());
    }
    // The decision that screenshots stay possible: the text of the secure-window flag is in none of the project's Kotlin, not
    // even in a comment, so the decision cannot drift silently.
    for (path, text) in &kotlin {
        for banned in ["FLAG_SECURE", "setSecure"] {
            if text.contains(banned) {
                problems.push(format!("{path} writes {banned}: screenshots must stay possible"));
            }
        }
    }
    let plugin = source_code(plugin_kt);
    if !plugin.contains("setRecentsScreenshotEnabled(!hidden)") {
        problems.push("the plugin does not hide the app switcher picture with setRecentsScreenshotEnabled".into());
    }
    if !plugin.contains("Build.VERSION.SDK_INT >= 33") {
        problems.push("the app switcher call is not guarded by the Android 13 check".into());
    }
    if !plugin.contains("\"unsupported\"") {
        problems.push("the plugin does not say that Android 12 has no app switcher protection".into());
    }

    let key = source_code(keystore_kt);
    for needed in [
        "KeyProperties.KEY_ALGORITHM_AES",
        "setKeySize(256)",
        "KeyProperties.BLOCK_MODE_GCM",
        "KeyProperties.ENCRYPTION_PADDING_NONE",
        "setUserAuthenticationRequired(true)",
        "setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL)",
        "setInvalidatedByBiometricEnrollment(false)",
        "setIsStrongBoxBacked(true)",
        "FEATURE_STRONGBOX_KEYSTORE",
        "BiometricPrompt.CryptoObject(cipher)",
        "BiometricManager.Authenticators.BIOMETRIC_STRONG or BiometricManager.Authenticators.DEVICE_CREDENTIAL",
        "KeyPermanentlyInvalidatedException",
        "isDeviceSecure",
        "AES/GCM/NoPadding",
        "KeyInfo",
        "SECURITY_LEVEL_STRONGBOX",
    ] {
        if !key.contains(needed) {
            problems.push(format!("KeystoreBridge.kt does not say {needed}"));
        }
    }
    // No timeout window, no weaker authentication, no library prompt, and no text of the system in an answer.
    if key.contains("setUserAuthenticationValidityDurationSeconds") {
        problems.push("KeystoreBridge.kt uses a validity duration: every use must need the person".into());
    }
    if key.contains("androidx.biometric") || !key.contains("BiometricPrompt.Builder(host)") {
        problems.push("KeystoreBridge.kt does not use the framework BiometricPrompt alone".into());
    }
    if key.contains("errString.toString()") || key.contains("e.message") || key.contains("e.toString()") {
        problems.push("KeystoreBridge.kt puts text of the system into a failure".into());
    }
    for (name, text) in gradle {
        if text.contains("androidx.biometric") || text.contains("biometric:") {
            problems.push(format!("{name} depends on a biometric library"));
        }
    }

    // The self-test: the name of its trigger is in the debug source set only, and its Rust is compiled for debug builds only.
    for (path, text) in files {
        if text.contains(SELFTEST_TRIGGER) && !path.starts_with("app/src/debug/") {
            problems.push(format!("{path} holds the name of the self-test trigger outside the debug source set"));
        }
    }
    if !files
        .iter()
        .any(|(path, text)| path.starts_with("app/src/debug/") && text.contains(SELFTEST_TRIGGER))
    {
        problems.push("the debug source set does not hold the self-test trigger".into());
    }
    let release_hook = files
        .iter()
        .find(|(path, _)| path == "app/src/release/java/ch/holzcloud/holzwave/DebugHooks.kt");
    match release_hook {
        Some((_, text)) if text.contains("fun take(): String? = null") => {}
        _ => problems.push("the release source set does not hold the hook that never answers".into()),
    }
    let mod_code = source_code(mod_rs);
    if !mod_code.contains("#[cfg(all(target_os = \"android\", debug_assertions))]\npub mod selftest;") {
        problems.push("the self-test module is not compiled for debug builds only".into());
    }
    problems
}

fn gradle_texts(files: &[(String, String)]) -> Vec<(String, String)> {
    files
        .iter()
        .filter(|(path, _)| path.ends_with(".gradle.kts") || path.ends_with(".gradle"))
        .cloned()
        .collect()
}

#[test]
fn the_secret_store_is_the_keystore_the_system_prompt_and_nothing_hand_made() {
    let files = project_text();
    let gradle = gradle_texts(&files);
    assert_none(
        "secret store",
        secret_store_problems(&files, KEYSTORE_KT, PLUGIN_KT, ANDROID_MOD_RS, &gradle),
    );
}

#[test]
fn the_project_never_makes_its_window_secure_and_hides_the_app_switcher_picture() {
    let files = project_text();
    let kotlin = own_kotlin(&files);
    assert!(kotlin.len() >= 5, "the project's Kotlin was found: {}", kotlin.len());
    for (path, text) in &kotlin {
        assert!(
            !text.contains("FLAG_SECURE") && !text.contains("setSecure"),
            "{path} makes the window secure"
        );
    }
    assert!(
        kotlin.iter().any(|(_, text)| text.contains("setRecentsScreenshotEnabled")),
        "the app switcher call is in the project"
    );
}

#[test]
fn the_secret_store_check_fails_on_bad_input() {
    let files = project_text();
    let gradle = gradle_texts(&files);
    let ok = |files: &[(String, String)], key: &str, plugin: &str, rs: &str, gradle: &[(String, String)]| {
        secret_store_problems(files, key, plugin, rs, gradle)
    };
    assert!(ok(&files, KEYSTORE_KT, PLUGIN_KT, ANDROID_MOD_RS, &gradle).is_empty(), "the project passes");

    // The secure-window flag, in a source or in a comment.
    for flag in ["window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)", "// not FLAG_SECURE", "window.setSecure(true)"] {
        let mut more = files.clone();
        more.push((
            "app/src/main/java/ch/holzcloud/holzwave/Other.kt".into(),
            format!("package x\n{flag}\n"),
        ));
        assert!(!ok(&more, KEYSTORE_KT, PLUGIN_KT, ANDROID_MOD_RS, &gradle).is_empty(), "{flag} passed");
    }
    // The picture is not hidden, or the call is not guarded.
    let no_call = PLUGIN_KT.replace("setRecentsScreenshotEnabled(!hidden)", "noop()");
    assert!(!ok(&files, KEYSTORE_KT, &no_call, ANDROID_MOD_RS, &gradle).is_empty(), "no call passed");
    let unguarded = PLUGIN_KT.replace("Build.VERSION.SDK_INT >= 33", "true");
    assert!(!ok(&files, KEYSTORE_KT, &unguarded, ANDROID_MOD_RS, &gradle).is_empty(), "unguarded passed");
    // A key that needs nobody, a time window, a library prompt, a message of the system in a failure.
    for (from, to) in [
        ("setUserAuthenticationRequired(true)", "setUserAuthenticationRequired(false)"),
        ("setUserAuthenticationParameters(0,", "setUserAuthenticationParameters(30,"),
        ("BiometricPrompt.CryptoObject(cipher)", "null"),
        ("setKeySize(256)", "setKeySize(128)"),
        ("setInvalidatedByBiometricEnrollment(false)", "setInvalidatedByBiometricEnrollment(true)"),
    ] {
        let weak = KEYSTORE_KT.replace(from, to);
        assert!(!ok(&files, &weak, PLUGIN_KT, ANDROID_MOD_RS, &gradle).is_empty(), "{from} passed");
    }
    let window = format!("{KEYSTORE_KT}\nval w = setUserAuthenticationValidityDurationSeconds(10)\n");
    assert!(!ok(&files, &window, PLUGIN_KT, ANDROID_MOD_RS, &gradle).is_empty(), "a window passed");
    let message = format!("{KEYSTORE_KT}\nval m = errString.toString()\n");
    assert!(!ok(&files, &message, PLUGIN_KT, ANDROID_MOD_RS, &gradle).is_empty(), "a message passed");
    let mut library = gradle.clone();
    library.push(("app/extra.gradle.kts".into(), "implementation(\"androidx.biometric:biometric:1.1.0\")".into()));
    assert!(!ok(&files, KEYSTORE_KT, PLUGIN_KT, ANDROID_MOD_RS, &library).is_empty(), "a library passed");
    // The trigger outside the debug source set, a self-test compiled into every build.
    let mut leaked = files.clone();
    leaked.push((
        "app/src/main/java/ch/holzcloud/holzwave/Leak.kt".into(),
        "val extra = \"holzwave_selftest\"".into(),
    ));
    assert!(!ok(&leaked, KEYSTORE_KT, PLUGIN_KT, ANDROID_MOD_RS, &gradle).is_empty(), "a leaked trigger passed");
    let always = ANDROID_MOD_RS.replace("#[cfg(all(target_os = \"android\", debug_assertions))]\npub mod selftest;", "pub mod selftest;");
    assert!(!ok(&files, KEYSTORE_KT, PLUGIN_KT, &always, &gradle).is_empty(), "an always-on self-test passed");
    let nothing: Vec<(String, String)> = Vec::new();
    assert!(!ok(&nothing, KEYSTORE_KT, PLUGIN_KT, ANDROID_MOD_RS, &gradle).is_empty(), "no sources passed");
}

// ---- HTTPS on Android: the platform certificate verifier, its Kotlin component and its set-up ------------------------------

const TLS_RS: &str = include_str!("../src/android/tls.rs");
const APP_CARGO_TOML: &str = include_str!("../Cargo.toml");
const CORE_CARGO_TOML: &str = include_str!("../../../core/Cargo.toml");
const HOST_CARGO_TOML: &str = include_str!("../../../host/Cargo.toml");
const CARGO_LOCK: &str = include_str!("../../../Cargo.lock");
const VERIFICATION_METADATA: &str = include_str!("../gen/android/gradle/verification-metadata.xml");

/// The place the verifier project publishes the Kotlin component, as a Maven repository.
const VERIFIER_REPOSITORY: &str =
    "https://github.com/rustls/rustls-platform-verifier/raw/maven-archive/android-release-support/maven/";

/// The versions that `Cargo.lock` holds for the package `name`.
fn lock_versions(lock: &str, name: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() == format!("name = \"{name}\"")
            && let Some(version) = lines.next().and_then(|next| next.trim().strip_prefix("version = \""))
        {
            found.push(version.trim_end_matches('"').to_owned());
        }
    }
    found
}

/// The text between the first `"` after `marker` and the next one.
fn quoted_after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let rest = &text[text.find(marker)? + marker.len()..];
    let rest = &rest[rest.find('"')? + 1..];
    Some(&rest[..rest.find('"')?])
}

/// The words that would make a request succeed against a certificate the system's trust store refuses, or that would bring a
/// second set of trust roots. None may be in the code of the shell, the host or the core: the platform verifier is the only one.
const TRUST_BYPASS_WORDS: &[&str] = &[
    ".dangerous()",
    "DangerousClientConfig",
    "impl ServerCertVerifier",
    "client::danger",
    "with_root_certificates",
    "with_custom_certificate_verifier",
    "webpki_roots",
    "webpki-roots",
    "RootCerts::WebPki",
    "RootCerts::Specific",
    "native_certs",
    "danger_accept_invalid",
    "accept_invalid_certs",
    "add_root_certificate",
    "tls_built_in_root_certs",
    "tls_built_in_webpki_certs",
    "disable_verification",
];

/// What the Gradle files, the lock, the rules and the sources must say for HTTPS to work on Android and to be judged by the
/// platform and by nothing else, one sentence per problem. `app_gradle_dir` is the folder of the Gradle project of the app,
/// used to resolve the relative path to the lock file, which must name the real `Cargo.lock`.
#[allow(clippy::too_many_arguments)]
fn tls_problems(
    app_gradle: &str,
    app_gradle_dir: &Path,
    rules: &str,
    lock: &str,
    verification: &str,
    plugin_kt: &str,
    tls_rs: &str,
    app_manifest: &str,
    other_manifests: &[(&str, &str)],
    sources: &[(String, String)],
) -> Vec<String> {
    let mut problems = Vec::new();
    let gradle = source_code(app_gradle);
    // The repository: the verifier project's own, asked for its own group only.
    if !gradle.contains(&format!("uri(\"{VERIFIER_REPOSITORY}\")")) {
        problems.push("the Gradle project does not name the verifier project's Maven repository".into());
    }
    if !gradle.contains("includeGroup(\"org.rustls\")") {
        problems.push("the verifier repository is not limited to the group org.rustls".into());
    }
    if gradle.matches("maven {").count() != 1 || gradle.contains("jitpack") || gradle.contains("mavenLocal") {
        problems.push("the app's Gradle file names a repository besides the verifier's".into());
    }
    // The version of the component is read from the lock file, so the Kotlin and the Rust half cannot drift.
    if !gradle.contains("const val CRATE_NAME = \"rustls-platform-verifier-android\"") {
        problems.push("the version source does not read the crate rustls-platform-verifier-android".into());
    }
    if !gradle.contains("useVersion(rustlsPlatformVerifierVersion.get())")
        || !gradle.contains("requested.group == \"org.rustls\" && requested.name == \"rustls-platform-verifier\"")
    {
        problems.push("the dependency's version is not taken from the lock file by the resolution rule".into());
    }
    match quoted_after(&gradle, "parameters.lockFile.set(layout.projectDirectory.file(") {
        None => problems.push("the version source has no path to the lock file".into()),
        Some(relative) => {
            let resolved = app_gradle_dir.join(relative);
            let expected = repository_root().join("Cargo.lock");
            match (resolved.canonicalize(), expected.canonicalize()) {
                (Ok(got), Ok(want)) if got == want => {}
                _ => problems.push(format!("the path {relative:?} from the Gradle project does not resolve to the repository's Cargo.lock")),
            }
        }
    }
    // The dependency has no version of its own, and the group is the one the repository serves (`org.rustls`).
    let dependency_lines: Vec<&str> = gradle.lines().filter(|line| line.contains("rustls-platform-verifier\"")).collect();
    if !dependency_lines.iter().any(|line| line.trim() == "implementation(\"org.rustls:rustls-platform-verifier\")") {
        problems.push("the dependency org.rustls:rustls-platform-verifier is not declared without a version".into());
    }
    if gradle.contains("org.rustls:rustls-platform-verifier:") || gradle.contains("\"rustls:rustls-platform-verifier") {
        problems.push("the verifier dependency has a static version or the wrong group".into());
    }
    // The shrinker keeps the component and the native method.
    if !rules.contains("-keep, includedescriptorclasses class org.rustls.platformverifier.** { *; }") {
        problems.push("the rules do not keep the verifier component".into());
    }
    let native_rule = rules.find("-keepclasseswithmembernames class ch.holzcloud.holzwave.HolzwavePlugin").map(|at| &rules[at..]);
    if !native_rule.is_some_and(|rule| rule[..rule.find('}').unwrap_or(rule.len())].contains("native <methods>;")) {
        problems.push("the rules do not keep the name of the native method of HolzwavePlugin".into());
    }
    // The lock names the Android crate, and the checksums of that version are pinned.
    let versions = lock_versions(lock, "rustls-platform-verifier-android");
    match versions.as_slice() {
        [version] => {
            let component = format!("<component group=\"org.rustls\" name=\"rustls-platform-verifier\" version=\"{version}\">");
            match verification.find(&component) {
                None => problems.push(format!("the verification metadata has no component for version {version}, the one in Cargo.lock")),
                Some(at) => {
                    let block = &verification[at..at + verification[at..].find("</component>").unwrap_or(0)];
                    for file in [format!("rustls-platform-verifier-{version}.aar"), format!("rustls-platform-verifier-{version}.pom")] {
                        let pinned = block.find(&format!("<artifact name=\"{file}\">")).is_some_and(|start| {
                            let rest = &block[start..];
                            let sum = quoted_after(rest, "<sha256 value=");
                            sum.is_some_and(|sum| sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()))
                        });
                        if !pinned {
                            problems.push(format!("the verification metadata does not pin a SHA-256 for {file}"));
                        }
                    }
                }
            }
        }
        other => problems.push(format!("Cargo.lock holds {} versions of rustls-platform-verifier-android, not one: {other:?}", other.len())),
    }
    if verification.matches("<trust ").count() != 1 || !verification.contains("<trust group=\"^(?!org\\.rustls$).*\" regex=\"true\"/>") {
        problems.push("the verification metadata does not trust every group but org.rustls, and nothing else".into());
    }
    if !verification.contains("<verify-metadata>true</verify-metadata>") {
        problems.push("the verification metadata does not verify the pom".into());
    }
    // The Kotlin side: one native method, called when the plugin is made, whose failure is not fatal.
    let plugin = source_code(plugin_kt);
    if !plugin.contains("private external fun initTls(context: Context)") {
        problems.push("HolzwavePlugin does not declare the native method initTls(context)".into());
    }
    match plugin.find("initTls(app)") {
        None => problems.push("HolzwavePlugin never calls initTls with the application context".into()),
        Some(at) => {
            let before = &plugin[..at];
            let try_at = before.rfind("try {");
            let init_at = before.rfind("init {");
            if try_at.is_none() || init_at.is_none() || try_at < init_at || !plugin[at..].contains("catch (e: Throwable)") {
                problems.push("initTls is not called inside a try block in the init block of the plugin".into());
            }
        }
    }
    // The Rust side: the exported name, the one call, the flag set only on success, the exception cleared on failure.
    let rust = source_code(tls_rs);
    if !rust.contains("#[jni_mangle(\"ch.holzcloud.holzwave.HolzwavePlugin\", \"initTls\")]") {
        problems.push("tls.rs does not export Java_ch_holzcloud_holzwave_HolzwavePlugin_initTls".into());
    }
    match (rust.find("platform_tls::init_with_env(env, context)"), rust.find("Ok(()) => {"), rust.find("tls_ready::global().set()"), rust.find("Err(_) => {")) {
        (Some(call), Some(ok), Some(set), Some(err)) if call < ok && ok < set && set < err => {}
        _ => problems.push("tls.rs does not set the readiness flag in the success arm of the one call, before the failure arm".into()),
    }
    if rust.matches("tls_ready::global().set()").count() != 1 {
        problems.push("the readiness flag is set in more than one place".into());
    }
    if !rust.contains("exception_clear()") {
        problems.push("tls.rs does not clear a pending Java exception after a failed set-up".into());
    }
    if rust.contains("unsafe") {
        problems.push("tls.rs has a line with the unsafe keyword".into());
    }
    // The dependencies: jni only, exactly pinned to the version the lock holds for the verifier, Android only.
    let jni_in_lock = lock_versions(lock, "jni");
    let table = "[target.'cfg(target_os = \"android\")'.dependencies]";
    match app_manifest.find(table) {
        None => problems.push("the app manifest has no Android-only dependency table".into()),
        Some(at) => {
            let rest = &app_manifest[at + table.len()..];
            let body = &rest[..rest.find("\n[").unwrap_or(rest.len())];
            let names: Vec<&str> = body.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).filter_map(|l| l.split('=').next()).map(str::trim).collect();
            if names != ["jni"] {
                problems.push(format!("the Android-only table names {names:?}, not only jni"));
            }
            let pin = quoted_after(body, "jni = ").unwrap_or("");
            let wanted = jni_in_lock.iter().find(|v| v.starts_with("0.22.")).cloned().unwrap_or_default();
            if pin != format!("={wanted}") || wanted.is_empty() {
                problems.push(format!("jni is pinned as {pin:?}, not exactly the 0.22 version the lock holds ({wanted:?})"));
            }
        }
    }
    let outside = app_manifest.replace(table, "");
    let outside_jni = outside.lines().filter(|l| l.trim_start().starts_with("jni")).count();
    if outside_jni != 1 {
        problems.push(format!("jni appears {outside_jni} times in the app manifest outside comments, not once"));
    }
    for (name, manifest) in other_manifests {
        if manifest.lines().any(|l| l.trim_start().starts_with("jni") || l.trim_start().starts_with("rustls-platform-verifier-android")) {
            problems.push(format!("{name} names jni or the Android verifier crate: only the app does"));
        }
    }
    if !app_manifest.contains("tauri = ") || app_manifest.lines().any(|l| l.trim_start().starts_with("tauri-plugin-")) {
        problems.push("the app manifest names a tauri-plugin crate".into());
    }
    // Nothing in the code bypasses the platform's judgment or brings another set of roots.
    for (path, text) in sources {
        let code = source_code(text);
        for word in TRUST_BYPASS_WORDS {
            if code.contains(word) {
                problems.push(format!("{path} contains {word:?}"));
            }
        }
    }
    problems
}

/// The sources of the shell, the host and the core, without their tests, as `(path, text)`.
fn tls_sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                out.push((path.display().to_string(), text));
            }
        }
    }
    let mut out = Vec::new();
    for dir in ["app/src-tauri/src", "host/src", "core/src"] {
        walk(&repository_root().join(dir), &mut out);
    }
    out
}

fn tls_check(
    mutate: impl Fn(&mut TlsInputs),
) -> Vec<String> {
    let mut inputs = TlsInputs {
        app_gradle: include_str!("../gen/android/app/build.gradle.kts").to_owned(),
        rules: APP_RULES.to_owned(),
        lock: CARGO_LOCK.to_owned(),
        verification: VERIFICATION_METADATA.to_owned(),
        plugin_kt: PLUGIN_KT.to_owned(),
        tls_rs: TLS_RS.to_owned(),
        app_manifest: APP_CARGO_TOML.to_owned(),
        sources: tls_sources(),
    };
    mutate(&mut inputs);
    let gradle_dir = project_root().join("app");
    tls_problems(
        &inputs.app_gradle,
        &gradle_dir,
        &inputs.rules,
        &inputs.lock,
        &inputs.verification,
        &inputs.plugin_kt,
        &inputs.tls_rs,
        &inputs.app_manifest,
        &[("core/Cargo.toml", CORE_CARGO_TOML), ("host/Cargo.toml", HOST_CARGO_TOML)],
        &inputs.sources,
    )
}

struct TlsInputs {
    app_gradle: String,
    rules: String,
    lock: String,
    verification: String,
    plugin_kt: String,
    tls_rs: String,
    app_manifest: String,
    sources: Vec<(String, String)>,
}

#[test]
fn https_on_android_is_the_platform_verifier_with_its_kotlin_component_set_up_once() {
    assert_none("tls", tls_check(|_| {}));
}

#[test]
fn the_sources_searched_for_a_trust_bypass_are_the_real_ones() {
    let sources = tls_sources();
    assert!(sources.len() > 50, "the sources were found: {}", sources.len());
    for needed in ["core/src/ai/http.rs", "core/src/sync/webdav.rs", "app/src-tauri/src/android/tls.rs", "host/src/sync.rs"] {
        assert!(sources.iter().any(|(path, _)| path.replace('\\', "/").ends_with(needed)), "{needed} is searched");
    }
    // Both clients choose the platform verifier, which is the thing the scan protects.
    let http = &sources.iter().find(|(p, _)| p.ends_with("core/src/ai/http.rs")).expect("http.rs").1;
    let webdav = &sources.iter().find(|(p, _)| p.ends_with("core/src/sync/webdav.rs")).expect("webdav.rs").1;
    assert!(http.contains("with_platform_verifier()"));
    assert!(webdav.contains("RootCerts::PlatformVerifier"));
}

#[test]
fn the_tls_check_fails_on_bad_input() {
    let version = lock_versions(CARGO_LOCK, "rustls-platform-verifier-android").remove(0);
    let cases: Vec<(&str, Box<dyn Fn(&mut TlsInputs)>)> = vec![
        ("no repository", Box::new(|i| i.app_gradle = i.app_gradle.replace(VERIFIER_REPOSITORY, "https://example.invalid/"))),
        ("repository for every group", Box::new(|i| i.app_gradle = i.app_gradle.replace("includeGroup(\"org.rustls\")", ""))),
        ("a second repository", Box::new(|i| i.app_gradle.push_str("\nrepositories { maven { url = uri(\"https://example.invalid/\") } }\n"))),
        ("a static version", Box::new(|i| i.app_gradle = i.app_gradle.replace("\"org.rustls:rustls-platform-verifier\"", "\"org.rustls:rustls-platform-verifier:0.1.0\""))),
        ("the wrong group", Box::new(|i| i.app_gradle = i.app_gradle.replace("implementation(\"org.rustls:", "implementation(\"rustls:"))),
        ("no dependency", Box::new(|i| i.app_gradle = i.app_gradle.replace("implementation(\"org.rustls:rustls-platform-verifier\")", ""))),
        ("a path to no lock file", Box::new(|i| i.app_gradle = i.app_gradle.replace("../../../../../Cargo.lock", "../../../../Cargo.lock"))),
        ("another crate read", Box::new(|i| i.app_gradle = i.app_gradle.replace("const val CRATE_NAME = \"rustls-platform-verifier-android\"", "const val CRATE_NAME = \"x\""))),
        ("no rule for the component", Box::new(|i| i.rules = i.rules.replace("org.rustls.platformverifier", "org.other"))),
        ("no rule for the native method", Box::new(|i| i.rules = i.rules.replace("native <methods>;", ""))),
        ("a lock without the crate", Box::new(|i| i.lock = i.lock.replace("name = \"rustls-platform-verifier-android\"", "name = \"other\""))),
        ("another version in the metadata", Box::new(move |i| i.verification = i.verification.replace(&format!("version=\"{version}\""), "version=\"9.9.9\""))),
        ("a short checksum", Box::new(|i| i.verification = i.verification.replace("aa021794230fbc2f", "aa02179"))),
        ("everything trusted", Box::new(|i| i.verification = i.verification.replace("^(?!org\\.rustls$).*", ".*"))),
        ("no call of initTls", Box::new(|i| i.plugin_kt = i.plugin_kt.replace("initTls(app)", "Unit"))),
        ("no native declaration", Box::new(|i| i.plugin_kt = i.plugin_kt.replace("private external fun initTls(context: Context)", ""))),
        ("an unguarded call", Box::new(|i| i.plugin_kt = i.plugin_kt.replace("catch (e: Throwable)", "catch (e: IllegalStateException)"))),
        ("another exported name", Box::new(|i| i.tls_rs = i.tls_rs.replace("\"initTls\"", "\"init\""))),
        ("a flag set before the call", Box::new(|i| i.tls_rs = i.tls_rs.replace("Err(_) => {", "Err(_) => {\n holzwave_host::tls_ready::global().set();"))),
        ("no exception cleared", Box::new(|i| i.tls_rs = i.tls_rs.replace("env.exception_clear();", ""))),
        ("an unsafe line", Box::new(|i| i.tls_rs.push_str("\nunsafe fn x() {}\n"))),
        ("jni outside the Android table", Box::new(|i| i.app_manifest = i.app_manifest.replace("[target.'cfg(target_os = \"android\")'.dependencies]\njni = \"=0.22.4\"", "[dependencies]\njni = \"=0.22.4\""))),
        ("jni with a range", Box::new(|i| i.app_manifest = i.app_manifest.replace("jni = \"=0.22.4\"", "jni = \"0.22\""))),
        ("a second Android dependency", Box::new(|i| i.app_manifest = i.app_manifest.replace("jni = \"=0.22.4\"", "jni = \"=0.22.4\"\nrustls-platform-verifier = \"0.7.1\""))),
        ("a tauri plugin", Box::new(|i| i.app_manifest.push_str("\ntauri-plugin-x = \"1\"\n"))),
        ("a custom verifier", Box::new(|i| i.sources.push(("x.rs".into(), "let c = builder.dangerous();".into())))),
        ("webpki roots", Box::new(|i| i.sources.push(("x.rs".into(), "use webpki_roots::TLS_SERVER_ROOTS;".into())))),
        ("an accept-all setting", Box::new(|i| i.sources.push(("x.rs".into(), "client.danger_accept_invalid_certs(true);".into())))),
    ];
    for (what, mutate) in &cases {
        assert!(!tls_check(mutate).is_empty(), "{what} passed");
    }
    // Comments may talk about these things.
    let commented = tls_check(|i| i.sources.push(("x.rs".into(), "// no webpki_roots and no .dangerous() here\n".into())));
    assert!(commented.is_empty(), "{commented:?}");
}

// ---- the single-file hand-over and the clipboard: the system's own dialogs, no storage permission, no grant kept --------------

const FILES_KT: &str =
    include_str!("../gen/android/app/src/main/java/ch/holzcloud/holzwave/HolzwaveFiles.kt");

/// The permissions that would let the app read the shared storage, the media of other apps or the list of installed apps. A copy
/// goes through the system's own document dialog, which needs none of them.
const STORAGE_PERMISSIONS: &[&str] = &[
    "READ_EXTERNAL_STORAGE",
    "WRITE_EXTERNAL_STORAGE",
    "MANAGE_EXTERNAL_STORAGE",
    "READ_MEDIA_IMAGES",
    "READ_MEDIA_VIDEO",
    "READ_MEDIA_AUDIO",
    "READ_MEDIA_VISUAL_USER_SELECTED",
    "QUERY_ALL_PACKAGES",
];

/// What the project must say about handing one file over, one sentence per problem. No manifest names a storage or media
/// permission or a `provider` element; nothing in the project opens a folder dialog or keeps a grant to a document; only a
/// `content` address from the dialog is used; the two dialogs and the clipboard are asked for in the code; the code never
/// turns a clip into text by `coerceToText`, never listens for changes of the clipboard and never logs.
fn file_dialog_problems(files: &[(String, String)], plugin_kt: &str, files_kt: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, manifest) in manifests(files) {
        for tag in tags(manifest) {
            if tag.name == "provider" {
                problems.push(format!("{name} declares a provider element"));
            }
            if tag.name.starts_with("uses-permission")
                && let Some(permission) = tag.attr("android:name")
                && STORAGE_PERMISSIONS.iter().any(|word| permission.ends_with(word))
            {
                problems.push(format!("{name} asks for {permission}"));
            }
        }
    }
    for (path, text) in files {
        for word in ["takePersistableUriPermission", "ACTION_OPEN_DOCUMENT_TREE"] {
            if text.contains(word) {
                problems.push(format!("{path} contains {word}: a folder or a kept grant is what the hand-over does without"));
            }
        }
    }
    let plugin = source_code(plugin_kt);
    let files_code = source_code(files_kt);
    for needed in ["Intent.ACTION_OPEN_DOCUMENT)", "Intent.ACTION_CREATE_DOCUMENT)", "Intent.CATEGORY_OPENABLE"] {
        if !plugin.contains(needed) {
            problems.push(format!("the plugin does not use {needed}"));
        }
    }
    // The folder a path from Rust must point into is the shell's own, with the name the host gives it.
    let host_rs = include_str!("../src/android/host.rs");
    if !host_rs.contains("pub const HOME_FOLDER: &str = \"home\";") || !files_code.contains("const val HOME_FOLDER = \"home\"") {
        problems.push("the shell's folder is not named the same in the host and in the Kotlin jail".into());
    }
    if plugin.contains("filesDir") || files_code.contains("filesDir") {
        problems.push("a path is checked against the files folder, not against the shell's own folder".into());
    }
    if !plugin.contains("HolzwaveFiles.shellHome(app.dataDir)") {
        problems.push("the jail of the paths is not the shell's folder".into());
    }
    if !plugin.contains("EXTRA_ALLOW_MULTIPLE, false") {
        problems.push("the open dialog does not say one document".into());
    }
    if !files_code.contains("uri.scheme == ContentResolver.SCHEME_CONTENT") {
        problems.push("a result of the dialog is not limited to a content address".into());
    }
    if plugin.matches("HolzwaveFiles.isContent(uri)").count() != 2 {
        problems.push("both dialog results are not checked for a content address".into());
    }
    if !files_code.contains("EXTRA_IS_SENSITIVE") || !files_code.contains("Build.VERSION.SDK_INT >= 33") {
        problems.push("a copied text is not marked sensitive from Android 13, guarded by the version".into());
    }
    if !files_code.contains("clip.getItemAt(0).text") || !files_code.contains("MIMETYPE_TEXT_PLAIN") {
        problems.push("the clipboard is not read as the plain text of its first item".into());
    }
    for (path, text) in own_kotlin(files) {
        let code = source_code(text);
        for word in ["coerceToText", "addPrimaryClipChangedListener", "htmlText", "getItemAt(0).uri", "getItemAt(0).intent"] {
            if code.contains(word) {
                problems.push(format!("{path} uses {word}: the clipboard is plain text only and nobody listens to it"));
            }
        }
    }
    if files_code.contains("Log.") || files_code.contains("println") || files_code.contains("printStackTrace") {
        problems.push("the file and clipboard code logs".into());
    }
    problems
}

#[test]
fn the_hand_over_uses_the_system_dialogs_and_asks_for_no_storage_permission_and_keeps_no_grant() {
    let files = project_text();
    assert!(
        files.iter().any(|(path, _)| path.ends_with("HolzwaveFiles.kt")),
        "the project's file code was found"
    );
    assert_none("hand-over", file_dialog_problems(&files, PLUGIN_KT, FILES_KT));
    // Nothing was added to the permission list for the dialogs: no storage, media or document permission is listed (the sixth line is the
    // local network, which has nothing to do with files).
    let listed = listed_permissions(PERMISSION_LIST);
    assert_eq!(listed.len(), 6, "{listed:?}");
    for permission in &listed {
        assert!(
            !permission.contains("STORAGE") && !permission.contains("MEDIA") && !permission.contains("DOCUMENT"),
            "{permission} is a permission for files"
        );
    }
}

#[test]
fn the_hand_over_check_fails_on_bad_input() {
    let files = project_text();
    assert!(file_dialog_problems(&files, PLUGIN_KT, FILES_KT).is_empty(), "the project passes");
    // A storage or media permission in any manifest of the project.
    for word in STORAGE_PERMISSIONS {
        let mut more = files.clone();
        more.push((
            "app/src/main/AndroidManifest.xml".into(),
            format!("<manifest><uses-permission android:name=\"android.permission.{word}\" /></manifest>"),
        ));
        assert!(!file_dialog_problems(&more, PLUGIN_KT, FILES_KT).is_empty(), "{word} passed");
    }
    // A provider element.
    let mut provider = files.clone();
    provider.push((
        "app/src/debug/AndroidManifest.xml".into(),
        "<manifest><application><provider android:name=\"x\" /></application></manifest>".into(),
    ));
    assert!(!file_dialog_problems(&provider, PLUGIN_KT, FILES_KT).is_empty(), "a provider passed");
    // A folder dialog and a kept grant, in code or in a comment.
    for text in [
        "val i = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)",
        "// no ACTION_OPEN_DOCUMENT_TREE",
        "resolver.takePersistableUriPermission(uri, 1)",
    ] {
        let mut more = files.clone();
        more.push(("app/src/main/java/ch/holzcloud/holzwave/Other.kt".into(), format!("package x\n{text}\n")));
        assert!(!file_dialog_problems(&more, PLUGIN_KT, FILES_KT).is_empty(), "{text} passed");
    }
    // The dialogs or their limits taken out.
    for (from, to) in [
        ("Intent.ACTION_OPEN_DOCUMENT)", "Intent.ACTION_GET_CONTENT)"),
        ("Intent.ACTION_CREATE_DOCUMENT)", "Intent.ACTION_SEND)"),
        ("Intent.CATEGORY_OPENABLE", "Intent.CATEGORY_DEFAULT"),
        ("EXTRA_ALLOW_MULTIPLE, false", "EXTRA_ALLOW_MULTIPLE, true"),
        ("HolzwaveFiles.isContent(uri)", "true"),
        ("HolzwaveFiles.shellHome(app.dataDir)", "host.filesDir"),
    ] {
        let changed = PLUGIN_KT.replace(from, to);
        assert!(!file_dialog_problems(&files, &changed, FILES_KT).is_empty(), "{from} passed");
    }
    // The content address check, the sensitive mark, the plain text read, a log of a text.
    for (from, to) in [
        ("uri.scheme == ContentResolver.SCHEME_CONTENT", "uri.scheme != null"),
        ("Build.VERSION.SDK_INT >= 33", "true"),
        ("clip.getItemAt(0).text", "clip.getItemAt(0).coerceToText(null)"),
        ("ClipDescription.MIMETYPE_TEXT_PLAIN", "ClipDescription.MIMETYPE_TEXT_HTML"),
    ] {
        let changed = FILES_KT.replace(from, to);
        assert!(!file_dialog_problems(&files, PLUGIN_KT, &changed).is_empty(), "{from} passed");
    }
    let renamed = FILES_KT.replace("const val HOME_FOLDER = \"home\"", "const val HOME_FOLDER = \"shell\"");
    assert!(!file_dialog_problems(&files, PLUGIN_KT, &renamed).is_empty(), "another folder name passed");
    let logs = format!("{FILES_KT}\nfun leak(t: String) {{ Log.d(\"x\", t) }}\n");
    assert!(!file_dialog_problems(&files, PLUGIN_KT, &logs).is_empty(), "a log passed");
    let mut listener = files.clone();
    listener.push((
        "app/src/main/java/ch/holzcloud/holzwave/Other.kt".into(),
        "package x\nclipboard.addPrimaryClipChangedListener(l)\n".into(),
    ));
    assert!(!file_dialog_problems(&listener, PLUGIN_KT, FILES_KT).is_empty(), "a listener passed");
    // A comment may name the words the code does not use.
    let mut commented = files.clone();
    commented.push((
        "app/src/main/java/ch/holzcloud/holzwave/Other.kt".into(),
        "package x\n// never coerceToText, never a listener\n".into(),
    ));
    assert!(file_dialog_problems(&commented, PLUGIN_KT, FILES_KT).is_empty(), "a comment about the words passed as bad");
}

// ---- the system bars: the heights the shell measured reach the page as four CSS variables, and the page uses them -------------

const INSETS_KT: &str =
    include_str!("../gen/android/app/src/main/java/ch/holzcloud/holzwave/SystemInsets.kt");
const MAIN_ACTIVITY_KT: &str =
    include_str!("../gen/android/app/src/main/java/ch/holzcloud/holzwave/MainActivity.kt");
const APP_CSS: &str = include_str!("../../src/app.css");
const PHONE_CSS: &str = include_str!("../../src/phone.css");

/// What the project must say so that the page's bars clear the system's on a WebView that reports no safe area: the shell writes
/// four bounded numbers into the root element (nothing else reaches the script, and the page gets no bridge object), the activity
/// installs it, a finished page load asks for it again, and the page takes the larger of its own `env()` and the shell's value.
fn insets_problems(insets_kt: &str, main_kt: &str, plugin_kt: &str, plugin_rs: &str, app_css: &str, phone_css: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let insets = source_code(insets_kt);
    for name in ["top", "right", "bottom", "left"] {
        if !insets.contains(&format!("\"{name}\"")) {
            problems.push(format!("SystemInsets.kt does not write --hw-sys-{name}"));
        }
        let token = format!("--hw-inset-{name}: max(env(safe-area-inset-{name}, 0px), var(--hw-sys-{name}, 0px));");
        if !app_css.contains(&token) {
            problems.push(format!("app.css does not take the larger of env() and --hw-sys-{name} for --hw-inset-{name}"));
        }
    }
    if !insets.contains("--hw-sys-%s") || !insets.contains("coerceIn(0, MAX_CSS_PX)") {
        problems.push("SystemInsets.kt does not bound the numbers it writes".into());
    }
    for banned in ["addJavascriptInterface", "loadUrl(", "eval(", "append("] {
        if insets.contains(banned) {
            problems.push(format!("SystemInsets.kt has {banned}: more than four numbers could reach the page"));
        }
    }
    if !source_code(main_kt).contains("SystemInsets.install(webView)") {
        problems.push("MainActivity does not install the insets in onWebViewCreate".into());
    }
    if !source_code(plugin_kt).contains("fun insetsApply(invoke: Invoke)") {
        problems.push("the plugin has no command insetsApply".into());
    }
    let rust = source_code(plugin_rs);
    if !rust.contains("PageLoadEvent::Finished") || !rust.contains("const INSETS_APPLY: &str = \"insetsApply\"") {
        problems.push("plugin.rs does not ask for the insets again when a page has finished loading".into());
    }
    if phone_css.contains("env(safe-area-inset") {
        problems.push("phone.css reads env(safe-area-inset-*) itself: it must use --hw-inset-*, which also has the shell's value".into());
    }
    for edge in ["top", "bottom"] {
        if !phone_css.contains(&format!("var(--hw-inset-{edge})")) {
            problems.push(format!("phone.css does not use --hw-inset-{edge}"));
        }
    }
    problems
}

#[test]
fn the_page_clears_the_system_bars_with_the_heights_the_shell_measured() {
    assert_none("the system bars", insets_problems(INSETS_KT, MAIN_ACTIVITY_KT, PLUGIN_KT, PLUGIN_RS, APP_CSS, PHONE_CSS));
}

#[test]
fn the_system_bars_check_fails_on_bad_input() {
    let ok = |insets: &str, main: &str, plugin: &str, rust: &str, app: &str, phone: &str| insets_problems(insets, main, plugin, rust, app, phone);
    assert!(ok(INSETS_KT, MAIN_ACTIVITY_KT, PLUGIN_KT, PLUGIN_RS, APP_CSS, PHONE_CSS).is_empty());
    let bad = [
        ("a bridge object", ok(&format!("{INSETS_KT}\nwebView.addJavascriptInterface(o, \"x\")\n"), MAIN_ACTIVITY_KT, PLUGIN_KT, PLUGIN_RS, APP_CSS, PHONE_CSS)),
        ("unbounded numbers", ok(&INSETS_KT.replace("coerceIn(0, MAX_CSS_PX)", "coerceAtLeast(0)"), MAIN_ACTIVITY_KT, PLUGIN_KT, PLUGIN_RS, APP_CSS, PHONE_CSS)),
        ("an edge missing", ok(&INSETS_KT.replace("\"left\"", "\"start\""), MAIN_ACTIVITY_KT, PLUGIN_KT, PLUGIN_RS, APP_CSS, PHONE_CSS)),
        ("not installed", ok(INSETS_KT, &MAIN_ACTIVITY_KT.replace("SystemInsets.install(webView)", ""), PLUGIN_KT, PLUGIN_RS, APP_CSS, PHONE_CSS)),
        ("no command", ok(INSETS_KT, MAIN_ACTIVITY_KT, &PLUGIN_KT.replace("fun insetsApply", "fun other"), PLUGIN_RS, APP_CSS, PHONE_CSS)),
        ("no page-load hook", ok(INSETS_KT, MAIN_ACTIVITY_KT, PLUGIN_KT, &PLUGIN_RS.replace("PageLoadEvent::Finished", "PageLoadEvent::Started"), APP_CSS, PHONE_CSS)),
        ("env alone in the token", ok(INSETS_KT, MAIN_ACTIVITY_KT, PLUGIN_KT, PLUGIN_RS, &APP_CSS.replace("max(env(safe-area-inset-bottom, 0px), var(--hw-sys-bottom, 0px))", "env(safe-area-inset-bottom, 0px)"), PHONE_CSS)),
        ("env in the phone sheet", ok(INSETS_KT, MAIN_ACTIVITY_KT, PLUGIN_KT, PLUGIN_RS, APP_CSS, &PHONE_CSS.replace("var(--hw-inset-bottom)", "env(safe-area-inset-bottom, 0px)"))),
    ];
    for (what, problems) in bad {
        assert!(!problems.is_empty(), "{what} passed");
    }
    // A comment may name what the code must not do.
    let commented = format!("{INSETS_KT}\n// never addJavascriptInterface\n");
    assert!(ok(&commented, MAIN_ACTIVITY_KT, PLUGIN_KT, PLUGIN_RS, APP_CSS, PHONE_CSS).is_empty());
}

// ---- the plugin and the activity the system relaunches ----------------------------------------------------------------------------

/// The plugin is made once per process with the first activity, and the system makes a new activity when it relaunches the window.
/// So the plugin must not keep the activity it was made with and ask it about the window: it keeps the application context and the
/// activity in use, which `onResume` sets and `onDestroy` clears, and the check for a visible app reads that one.
fn relaunch_problems(plugin_kt: &str, keystore_kt: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let plugin = source_code(plugin_kt);
    if !plugin.contains("class HolzwavePlugin(host: Activity) : Plugin(host)") {
        problems.push("HolzwavePlugin keeps the activity it was made with as a property".into());
    }
    let after_constructor = plugin.split("class HolzwavePlugin(host: Activity)").nth(1).unwrap_or("");
    let uses = after_constructor
        .lines()
        .filter(|line| line.contains("host.") && !line.contains("val app: Context = host.applicationContext") && !line.contains("Activity? = host"))
        .count();
    if uses != 0 {
        problems.push(format!("HolzwavePlugin reads the activity it was made with in {uses} place(s)"));
    }
    for needed in [
        "private var current: Activity? = host",
        "current = activity",
        "if (current === activity) current = null",
        "(current as? LifecycleOwner)?.lifecycle",
        "KeystoreBridge(app) { current }",
    ] {
        if !plugin.contains(needed) {
            problems.push(format!("HolzwavePlugin lacks `{needed}`"));
        }
    }
    if !source_code(keystore_kt).contains("val host = activityOf()") {
        problems.push("the secret store's prompt does not ask for the activity in use".into());
    }
    problems
}

#[test]
fn the_plugin_asks_the_activity_in_use_and_not_the_one_it_was_made_with() {
    assert_none("the relaunched window", relaunch_problems(PLUGIN_KT, KEYSTORE_KT));
}

#[test]
fn the_relaunch_check_fails_on_bad_input() {
    assert!(relaunch_problems(PLUGIN_KT, KEYSTORE_KT).is_empty());
    let bad = [
        ("a kept activity", PLUGIN_KT.replace("class HolzwavePlugin(host: Activity)", "class HolzwavePlugin(private val host: Activity)")),
        ("the old visibility read", PLUGIN_KT.replace("(current as? LifecycleOwner)?.lifecycle", "(host as? LifecycleOwner)?.lifecycle")),
        ("no tracking in onResume", PLUGIN_KT.replace("current = activity\n", "")),
        ("no clearing in onDestroy", PLUGIN_KT.replace("if (current === activity) current = null", "")),
        ("a stray read of the first activity", format!("{PLUGIN_KT}\n  fun leak() = host.runOnUiThread {{ }}\n")),
    ];
    for (what, text) in bad {
        assert!(!relaunch_problems(&text, KEYSTORE_KT).is_empty(), "{what} passed");
    }
    let keystore = KEYSTORE_KT.replace("val host = activityOf()", "val host = first");
    assert!(!relaunch_problems(PLUGIN_KT, &keystore).is_empty(), "a prompt on the first activity passed");
}

// ---- the local network permission ------------------------------------------------------------------------------------------------

/// What the project must say about the permission to reach the local network: it is declared in the manifest and in the one list,
/// it is asked for by the plugin only when a connection needs it (a command, never at start), only on Android 17 and later, with the
/// system's dialog, and the Rust side installs the gate from setup and nowhere asks it at the start.
fn local_network_problems(manifest: &str, list: &str, plugin_kt: &str, lib_rs: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let declared = tags(manifest)
        .iter()
        .any(|tag| tag.name == "uses-permission" && tag.attr("android:name") == Some("android.permission.ACCESS_LOCAL_NETWORK"));
    if !declared {
        problems.push("the manifest does not declare ACCESS_LOCAL_NETWORK".into());
    }
    if !listed_permissions(list).contains(&"android.permission.ACCESS_LOCAL_NETWORK") {
        problems.push("android-permissions.txt does not list ACCESS_LOCAL_NETWORK".into());
    }
    let plugin = source_code(plugin_kt);
    for needed in [
        "fun localNetworkEnsure(invoke: Invoke)",
        "private const val LOCAL_NETWORK = \"android.permission.ACCESS_LOCAL_NETWORK\"",
        "private const val LOCAL_NETWORK_SDK = 37",
        "if (Build.VERSION.SDK_INT < LOCAL_NETWORK_SDK)",
        "PluginManager.requestPermissions(arrayOf(LOCAL_NETWORK))",
        "LOCAL_NETWORK_WAIT_MS",
    ] {
        if !plugin.contains(needed) {
            problems.push(format!("HolzwavePlugin lacks `{needed}`"));
        }
    }
    // Never at the start: no lifecycle hook and no init block asks for it.
    for hook in ["override fun onResume", "init {", "override fun load("] {
        if let Some(at) = plugin.find(hook) {
            let body: String = plugin[at..].lines().take(14).collect::<Vec<_>>().join("\n");
            if body.contains("LOCAL_NETWORK") || body.contains("localNetworkEnsure") {
                problems.push(format!("`{hook}` asks for the local network permission: it may only be asked when a connection needs it"));
            }
        }
    }
    let lib = source_code(lib_rs);
    if !lib.contains("android::network::install();") {
        problems.push("lib.rs does not install the gate for the local network".into());
    }
    if lib.contains("localNetworkEnsure") {
        problems.push("lib.rs asks for the permission itself".into());
    }
    problems
}

const LIB_RS: &str = include_str!("../src/lib.rs");

#[test]
fn the_local_network_permission_is_declared_and_asked_only_when_a_connection_needs_it() {
    let files = project_text();
    assert_none(
        "the local network permission",
        local_network_problems(find(&files, "app/src/main/AndroidManifest.xml"), PERMISSION_LIST, PLUGIN_KT, LIB_RS),
    );
}

#[test]
fn the_local_network_check_fails_on_bad_input() {
    let files = project_text();
    let manifest = find(&files, "app/src/main/AndroidManifest.xml").to_owned();
    assert!(local_network_problems(&manifest, PERMISSION_LIST, PLUGIN_KT, LIB_RS).is_empty());
    let cases = [
        (
            "not declared",
            local_network_problems(&manifest.replace("android.permission.ACCESS_LOCAL_NETWORK", "android.permission.NEARBY"), PERMISSION_LIST, PLUGIN_KT, LIB_RS),
        ),
        (
            "not listed",
            local_network_problems(&manifest, &PERMISSION_LIST.replace("android.permission.ACCESS_LOCAL_NETWORK", ""), PLUGIN_KT, LIB_RS),
        ),
        (
            "asked on every Android",
            local_network_problems(&manifest, PERMISSION_LIST, &PLUGIN_KT.replace("if (Build.VERSION.SDK_INT < LOCAL_NETWORK_SDK)", "if (false)"), LIB_RS),
        ),
        (
            "asked on resume",
            local_network_problems(
                &manifest,
                PERMISSION_LIST,
                &PLUGIN_KT.replace("override fun onResume(activity: androidx.appcompat.app.AppCompatActivity) {", "override fun onResume(activity: androidx.appcompat.app.AppCompatActivity) {\n    PluginManager.requestPermissions(arrayOf(LOCAL_NETWORK)) { }"),
                LIB_RS,
            ),
        ),
        (
            "no gate",
            local_network_problems(&manifest, PERMISSION_LIST, PLUGIN_KT, &LIB_RS.replace("android::network::install();", "")),
        ),
    ];
    for (what, problems) in cases {
        assert!(!problems.is_empty(), "{what} passed");
    }
}
