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
    let (major, minor, patch) = (number("major", major, 209)?, number("minor", minor, 99)?, number("patch", patch, 99)?);
    let (stage, n) = match pre {
        None => (9, 0),
        Some(pre) => {
            let word_end = pre.find(|c: char| c.is_ascii_digit()).unwrap_or(pre.len());
            let (word, digits) = pre.split_at(word_end);
            let stage = match word {
                "alpha" => 1,
                "beta" => 2,
                "rc" => 3,
                _ => return Err(format!("the pre-release stage {word:?} of {version} is not alpha, beta or rc")),
            };
            let n = number("pre-release number", digits, 99)?;
            if n < 1 {
                return Err(format!("the pre-release number of {version} must be 1 to 99"));
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
        Err(why) => return vec![format!("the version {version} of the app has no version code: {why}")],
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
                for pre in ["alpha1", "alpha99", "beta1", "beta2", "beta99", "rc1", "rc99"] {
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
            let text = if pre.is_empty() { format!("{major}.{minor}.{patch}") } else { format!("{major}.{minor}.{patch}-{pre}") };
            version_code(&text).unwrap_or_else(|why| panic!("{text}: {why}"))
        })
        .collect();
    assert_eq!(versions.len(), 1000);
    for pair in codes.windows(2) {
        assert!(pair[0] < pair[1], "the codes are not strictly increasing: {} then {}", pair[0], pair[1]);
    }
}

#[test]
fn the_scheme_refuses_what_it_cannot_order() {
    for bad in [
        "0.0.100", "0.100.0", "210.0.0", "0.0.1-beta100", "0.0.1-beta0", "0.0.1-beta", "0.0.1-gamma1", "0.0.1-beta.2", "0.0.1+5",
        "0.0.1-beta2+x", "", "gamma", "1.0", "1.0.0.0", "0.01.0", "0.-1.0", "0.0.99999999999999999999",
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
        let output = Command::new("sh").arg(&script).arg(version).output().expect("sh runs the script");
        assert!(output.status.success(), "{version}: {}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), code.to_string(), "{version}");
    }
    for bad in ["0.0.100", "0.0.1-gamma1", "0.0.1+5", "", "0.0.1-beta0"] {
        let output = Command::new("sh").arg(&script).arg(bad).output().expect("sh runs the script");
        assert_eq!(output.status.code(), Some(2), "{bad:?} was not refused by the script");
        assert!(output.stdout.is_empty(), "{bad:?}: the script printed on stdout");
    }
}

#[test]
fn the_scheme_is_written_down_once_in_the_header_of_the_script() {
    let header: String = VERSION_SCRIPT.lines().take_while(|line| line.starts_with('#')).collect::<Vec<_>>().join("\n");
    for word in ["major * 10000000", "minor * 100000", "patch * 1000", "stage * 100", "alpha", "beta", "release candidate", "final", "209", "2100000000"] {
        assert!(header.contains(word), "the header of the version script does not say {word:?}");
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
    assert!(problems[0].contains("android-version-code.sh"), "{problems:?}");
    assert!(!version_code_problems(&tauri, &json("{}")).is_empty());
    assert!(!version_code_problems(&json(r#"{"version":"0.0.1-gamma1"}"#), &android).is_empty());
    assert!(!version_code_problems(&json("{}"), &android).is_empty());
    // A bump of the version without the code is the failure this test exists for.
    let bumped = json(r#"{"version":"0.0.1-beta3"}"#);
    assert!(!version_code_problems(&bumped, &android).is_empty());
    assert!(version_code_problems(&json(r#"{"version":"0.0.1-beta2"}"#), &android).is_empty());
    // The code is an integer in the file, not a string.
    assert!(!version_code_problems(&json(r#"{"version":"0.0.1-beta2"}"#), &json(r#"{"bundle":{"android":{"versionCode":"1202"}}}"#)).is_empty());
}
