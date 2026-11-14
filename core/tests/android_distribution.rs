//! The guard of the Android distribution page and the draft store listing.
//!
//! `docs/android-distribution.md` and the fastlane draft under `packaging/android/fastlane/metadata/android/en-US/` state
//! facts that the build must keep true: the one processor family, the permissions of the manifest and the subtype text of the
//! foreground service, the version code scheme, the anti-feature NonFreeNet, the limit of an app that is not registered with
//! Google, the missing key fingerprint and the length limits of the listing. These tests compare those facts with the files
//! they describe. They read the repository's own files (and run the version code script on unix) and nothing else.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent: the workspace root").to_path_buf()
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

const PAGE: &str = "docs/android-distribution.md";
const MANIFEST: &str = "app/src-tauri/gen/android/app/src/main/AndroidManifest.xml";
const LISTING: &str = "packaging/android/fastlane/metadata/android/en-US";

/// `text` with every run of whitespace turned into one space, so a sentence wrapped over lines still matches.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The short names (`INTERNET`, ...) of every `<uses-permission>` of the manifest, comments left out.
fn manifest_permissions(manifest: &str) -> BTreeSet<String> {
    let mut code = String::new();
    let mut rest = manifest;
    while let Some(start) = rest.find("<!--") {
        code.push_str(&rest[..start]);
        rest = rest[start..].find("-->").map_or("", |end| &rest[start + end + 3..]);
    }
    code.push_str(rest);
    code.split("<uses-permission")
        .skip(1)
        .filter_map(|tail| {
            let name = tail.split("android:name=\"").nth(1)?.split('"').next()?;
            Some(name.rsplit('.').next().unwrap_or(name).to_owned())
        })
        .collect()
}

#[test]
fn the_page_names_exactly_the_permissions_of_the_manifest_and_counts_them_right() {
    let page = read(PAGE);
    let permissions = manifest_permissions(&read(MANIFEST));
    assert!(!permissions.is_empty(), "the manifest declares no permission: the parser is wrong");
    for name in &permissions {
        assert!(page.contains(&format!("`{name}`")), "{PAGE} does not name the manifest permission `{name}`");
    }
    let words = ["zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];
    let word = words.get(permissions.len()).expect("fewer than eleven permissions");
    assert!(
        page.contains(&format!("**Permissions: exactly {word}.**")),
        "{PAGE} does not say `Permissions: exactly {word}.` for the {} permissions of the manifest",
        permissions.len()
    );
}

#[test]
fn the_page_quotes_the_subtype_text_of_the_foreground_service_and_its_type() {
    let manifest = read(MANIFEST);
    let subtype = manifest
        .split("PROPERTY_SPECIAL_USE_FGS_SUBTYPE\"")
        .nth(1)
        .and_then(|tail| tail.split("android:value=\"").nth(1))
        .and_then(|tail| tail.split('"').next())
        .expect("the manifest has a special-use subtype property");
    let page = flat(&read(PAGE));
    assert!(page.contains(&format!("`{}`", flat(subtype))), "{PAGE} does not quote the subtype text `{subtype}`");
    assert!(page.contains("`specialUse`"), "{PAGE} does not name the service type `specialUse`");
}

#[test]
fn the_page_says_only_arm64_v8a_is_built() {
    let page = read(PAGE);
    assert!(page.contains("Only arm64-v8a is built"), "{PAGE} does not say that only arm64-v8a is built");
    for other in ["armeabi-v7a is built", "x86_64 is built"] {
        assert!(!page.contains(other), "{PAGE} claims `{other}`");
    }
}

#[test]
fn the_version_code_example_of_the_page_is_what_the_script_prints() {
    let page = flat(&read(PAGE));
    assert!(page.contains("`0.0.1-beta2` gives 1202"), "{PAGE} lost the example `0.0.1-beta2` gives 1202");
    assert!(
        page.contains("`major * 10000000 + minor * 100000 + patch * 1000 + stage * 100 + number`"),
        "{PAGE} lost the formula of the version code"
    );
    #[cfg(unix)]
    {
        let out = std::process::Command::new("sh")
            .arg(root().join("scripts/android-version-code.sh"))
            .arg("0.0.1-beta2")
            .output()
            .expect("sh runs");
        assert!(out.status.success(), "the version code script failed: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1202", "the script disagrees with the page");
    }
}

#[test]
fn the_page_and_the_listing_name_the_anti_feature_and_the_registration_limit() {
    let page = flat(&read(PAGE));
    let listing = flat(&read(&format!("{LISTING}/full_description.txt")));
    for (name, text) in [(PAGE, &page), ("full_description.txt", &listing)] {
        for fact in ["NonFreeNet", "advanced flow", "ADB", "24 hours", "not registered"] {
            // The page says "Without registration"; the listing says "not registered".
            if fact == "not registered" && name == PAGE {
                assert!(text.contains("Without registration"), "{name} does not state the registration limit");
                continue;
            }
            assert!(text.contains(fact), "{name} does not say `{fact}`");
        }
    }
    assert!(page.contains("up to 20 devices"), "{PAGE} lost the device limit of a free account");
}

/// Whether `text` holds a SHA-256 digest: 64 hex digits, alone or as 32 colon-separated pairs.
fn has_digest(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == ':'))
        .any(|word| {
            let hex: String = word.chars().filter(|c| *c != ':').collect();
            hex.len() == 64 && (word.len() == 64 || word.len() == 95)
        })
}

#[test]
fn the_page_has_no_fingerprint_while_it_says_the_key_does_not_exist() {
    let page = read(PAGE);
    assert!(page.contains("apksigner verify --print-certs"), "{PAGE} lost how to check the key");
    assert!(page.contains("Signer #1 certificate SHA-256 digest"), "{PAGE} lost the line to compare");
    if flat(&page).contains("It does not exist yet, so there is no fingerprint on this page") {
        assert!(!has_digest(&page), "{PAGE} says the key does not exist but holds a digest");
    } else {
        assert!(has_digest(&page), "{PAGE} dropped the placeholder statement but publishes no fingerprint");
    }
}

#[test]
fn the_digest_check_sees_both_forms_and_skips_short_hex() {
    assert!(has_digest(&"ab".repeat(32)));
    assert!(has_digest(&vec!["AB"; 32].join(":")));
    assert!(!has_digest("deadbeef 1202 0.0.1"));
}

#[test]
fn the_listing_texts_keep_their_length_limits() {
    let short = read(&format!("{LISTING}/short_description.txt"));
    let title = read(&format!("{LISTING}/title.txt"));
    let full = read(&format!("{LISTING}/full_description.txt"));
    let short = short.trim_end();
    assert!(!short.is_empty() && short.chars().count() <= 80, "short_description.txt has {} characters", short.chars().count());
    assert!(!short.contains('\n'), "short_description.txt is more than one line");
    let title = title.trim_end();
    assert!(!title.is_empty() && title.chars().count() <= 30, "title.txt has {} characters", title.chars().count());
    assert!(full.chars().count() <= 4000, "full_description.txt has {} characters", full.chars().count());
    assert!(flat(&full).contains("This is a beta"), "full_description.txt does not say it is a beta");
}

#[test]
fn the_permission_parser_skips_comments_and_shortens_names() {
    let manifest = "<!-- <uses-permission android:name=\"android.permission.CAMERA\" /> -->\n\
                    <uses-permission android:name=\"android.permission.INTERNET\" />\n";
    assert_eq!(manifest_permissions(manifest), BTreeSet::from(["INTERNET".to_owned()]));
}
