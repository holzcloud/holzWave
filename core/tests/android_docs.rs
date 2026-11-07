//! The guard against public texts that claim more about Android than was run.
//!
//! Android was built and run on arm64 emulators and never on a phone or a tablet. These tests read the public texts and fail
//! when the README's Android row starts to promise a way to get the app, when a page drops the statement that only emulators
//! were used, when the test plan loses its Android part, or when a label of the private planning tree gets into one of the
//! files that describe Android. They read the repository's own files and nothing else; the Result column of the test plan is
//! never looked at (it is the owner's).

use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent: the workspace root").to_path_buf()
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Words that the README's Android row must not hold, as lower-case text. The README's row says what was built and what was
/// run; how and where an Android build is offered is the distribution page's business, and until a channel exists the row
/// must not read like one. The bare word `store` is allowed because the row names the secret store.
const ROW_FORBIDDEN: [&str; 8] = ["available", "released", "download", "install", "f-droid", "obtainium", "play store", "app store"];

/// The first line of `text` that starts a table row for Android.
fn android_row(text: &str) -> Option<&str> {
    text.lines().find(|line| line.starts_with("| Android"))
}

/// What is wrong with an Android row of the README, one sentence each: a forbidden word, a missing mention of the emulator,
/// or a missing link to the test plan.
fn row_problems(line: &str) -> Vec<String> {
    let lower = line.to_lowercase();
    let mut problems = Vec::new();
    for word in ROW_FORBIDDEN {
        if lower.contains(word) {
            problems.push(format!("the row says `{word}`"));
        }
    }
    if !lower.contains("emulator") {
        problems.push("the row does not name the emulator".to_owned());
    }
    if !line.contains("docs/test-plan.md") {
        problems.push("the row does not link docs/test-plan.md".to_owned());
    }
    problems
}

#[test]
fn the_readme_row_for_android_names_the_emulator_links_the_test_plan_and_promises_no_way_to_get_the_app() {
    let readme = read("README.md");
    let row = android_row(&readme).expect("README.md has a table row that starts with `| Android`");
    let problems = row_problems(row);
    assert!(problems.is_empty(), "README.md, the Android row: {problems:?}");
}

#[test]
fn the_check_of_the_android_row_fires_on_each_forbidden_word_and_accepts_an_honest_row() {
    let honest = "| Android | built; run on arm64 emulators only, never on a phone ([test plan](docs/test-plan.md)) |";
    assert!(row_problems(honest).is_empty(), "an honest row passes: {:?}", row_problems(honest));
    for word in ROW_FORBIDDEN {
        // Capitalised, so that the check is shown to be case-insensitive.
        let mut capitalised = word.to_owned();
        capitalised[..1].make_ascii_uppercase();
        let hostile = format!("| Android | built, run on an emulator, {capitalised} ([test plan](docs/test-plan.md)) |");
        assert_eq!(row_problems(&hostile), [format!("the row says `{word}`")], "the word `{word}` is caught");
    }
    assert_eq!(row_problems("| Android | built, tested on a phone |"), [
        "the row does not name the emulator".to_owned(),
        "the row does not link docs/test-plan.md".to_owned(),
    ]);
    // The secret store is named by the row, and the bare word is allowed.
    assert!(row_problems("| Android | an emulator ran the secret store ([test plan](docs/test-plan.md)) |").is_empty());
}

/// The blocks of `text` separated by a blank line.
fn paragraphs(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

/// Whether one paragraph of `text` names Android and the emulator and says that a phone or a tablet was not used.
fn says_emulator_only(text: &str) -> bool {
    paragraphs(text).iter().any(|block| {
        let lower = block.to_lowercase();
        let flat: String = lower.split_whitespace().collect::<Vec<_>>().join(" ");
        lower.contains("android")
            && lower.contains("emulator")
            && ["never on a phone", "not on a phone", "no phone", "never on a tablet", "not on a tablet", "no tablet"]
                .iter()
                .any(|phrase| flat.contains(phrase))
    })
}

#[test]
fn the_status_the_build_page_and_the_roadmap_each_say_that_only_emulators_were_used() {
    for file in ["docs/status.md", "docs/build-android.md", "docs/roadmap.md"] {
        assert!(
            says_emulator_only(&read(file)),
            "{file} has no paragraph that names Android and the emulator and says that no phone or tablet was used"
        );
    }
}

#[test]
fn the_check_of_the_emulator_statement_needs_all_three_parts_in_one_paragraph() {
    assert!(says_emulator_only("Android was run on arm64 emulators only, never on a phone or a tablet.\n"));
    // Split across a line break, and in capitals.
    assert!(says_emulator_only("ANDROID was run on an Emulator,\nnever on a\nphone.\n"));
    // The parts are in different paragraphs.
    assert!(!says_emulator_only("Android is built.\n\nIt ran on an emulator.\n\nNever on a phone.\n"));
    // No statement about the phone.
    assert!(!says_emulator_only("Android ran on an emulator and works well.\n"));
}

#[test]
fn the_test_plan_keeps_its_android_part_and_the_readme_links_it() {
    assert!(read("docs/test-plan.md").lines().any(|line| line == "## Part 13: Android"), "docs/test-plan.md lost the heading `## Part 13: Android`");
    assert!(read("README.md").contains("Part 13"), "README.md does not mention Part 13 of the test plan");
}

/// The files that describe Android, as the plan that wrote these texts touched them.
const TOUCHED: [&str; 8] = [
    "README.md",
    "docs/status.md",
    "docs/roadmap.md",
    "docs/architecture.md",
    "docs/building.md",
    "docs/config.md",
    "docs/build-android.md",
    "docs/test-plan.md",
];

/// The one-based line numbers and the text of every label of the private planning tree in `text`: a plan id of the Android
/// phase (two digits, a hyphen, two digits, alone), a threat id of it and the name of its folder. A decision id (`D-` and two
/// digits) is looked for only in a paragraph that names Android or the emulator, because older pages carry decision ids of
/// earlier phases that this guard does not own.
fn label_hits(text: &str) -> Vec<(usize, String)> {
    // Built from pieces, so that this file holds no label itself.
    let phase = String::from("0") + "9";
    let folder = phase.clone() + "-" + "mobile";
    let threat = format!("T-{phase}-");
    let mut hits = Vec::new();
    let mut android_paragraph = false;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            android_paragraph = false;
        }
        let lower = line.to_lowercase();
        if lower.contains("android") || lower.contains("emulator") {
            android_paragraph = true;
        }
        let chars: Vec<char> = line.chars().collect();
        let at = |i: usize, pattern: &str| -> bool {
            let want: Vec<char> = pattern.chars().collect();
            chars.len() >= i + want.len() && chars[i..i + want.len()] == want[..]
        };
        for i in 0..chars.len() {
            let before_ok = i == 0 || !(chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '-' | '_' | '.'));
            let two_digits = |at_index: usize| {
                chars.get(at_index).is_some_and(char::is_ascii_digit) && chars.get(at_index + 1).is_some_and(char::is_ascii_digit)
            };
            let not_followed_by_digit = |at_index: usize| !chars.get(at_index).is_some_and(char::is_ascii_digit);
            if at(i, &format!("{phase}-")) && before_ok && two_digits(i + 3) && not_followed_by_digit(i + 5) {
                hits.push((index + 1, chars[i..i + 5].iter().collect()));
            }
            if at(i, &threat) && chars.get(i + threat.chars().count()).is_some_and(char::is_ascii_digit) {
                hits.push((index + 1, chars[i..i + threat.chars().count() + 1].iter().collect()));
            }
            if at(i, &folder) {
                hits.push((index + 1, folder.clone()));
            }
            if android_paragraph && at(i, "D-") && before_ok && two_digits(i + 2) && not_followed_by_digit(i + 4) {
                hits.push((index + 1, chars[i..i + 4].iter().collect()));
            }
        }
    }
    hits
}

#[test]
fn no_text_that_describes_android_carries_a_label_of_the_private_planning_tree() {
    let mut problems = Vec::new();
    for file in TOUCHED {
        for (line, label) in label_hits(&read(file)) {
            problems.push(format!("{file}:{line}: {label}"));
        }
    }
    assert!(problems.is_empty(), "labels of the private planning tree in the texts about Android:\n{}", problems.join("\n"));
}

#[test]
fn the_label_check_sees_ids_threat_ids_decision_ids_and_the_folder_and_skips_dates_and_older_decision_ids() {
    let plan = String::from("0") + "9";
    let id = format!("{plan}-12");
    let hit = |text: &str| label_hits(text).into_iter().map(|(_, label)| label).collect::<Vec<_>>();
    assert_eq!(hit(&format!("see plan {id} for it")), [id.clone()], "a plan id");
    assert_eq!(hit(&format!("see ({id})")), [id.clone()], "a plan id in parentheses");
    assert!(hit(&format!("on 2026-{id}")).is_empty(), "inside a date");
    assert!(hit(&format!("x{id}")).is_empty(), "after a letter");
    assert!(hit(&format!("{id}3")).is_empty(), "before a third digit");
    assert_eq!(hit(&format!("a bound (T-{plan}-024)")), [format!("T-{plan}-0")], "a threat id");
    assert_eq!(hit(&format!("the folder {plan}-mobile/")), [format!("{plan}-mobile")], "the folder name");
    assert_eq!(hit("Android was run on an emulator (D-08)"), ["D-08".to_owned()], "a decision id next to Android");
    assert!(hit("the engine sees the type (D-05)").is_empty(), "an older decision id away from Android is not this guard's");
    assert_eq!(label_hits("one\ntwo\nAndroid (D-24)")[0].0, 3, "the line number is one-based");
}
