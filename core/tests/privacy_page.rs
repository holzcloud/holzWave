//! The privacy statement (`PRIVACY.md`) says what is true and nothing more.
//!
//! The statement is what a store reviewer and an ordinary person read as the developer's commitment, so it may not promise
//! more than the repository pins. These tests read the statement and the Privacy section of `docs/config.md` as text
//! (std only, no crate) and compare their key facts, so a change to one fails until the other follows. The matcher has an
//! in-memory self-test: a statement with a key sentence removed must fail.

const PRIVACY: &str = include_str!("../../PRIVACY.md");
const CONFIG: &str = include_str!("../../docs/config.md");

/// The text with every run of white space made one blank, so that a phrase that wraps a line still matches.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The phrases of `phrases` that `text` does not hold, compared without case and across line breaks.
fn missing<'a>(text: &str, phrases: &[&'a str]) -> Vec<&'a str> {
    let haystack = flat(text).to_lowercase();
    phrases.iter().copied().filter(|p| !haystack.contains(&p.to_lowercase())).collect()
}

/// The text of the `## <title>` section of a Markdown file, up to the next `## ` heading.
fn section<'a>(text: &'a str, title: &str) -> &'a str {
    let heading = format!("## {title}");
    let start = text.find(&format!("\n{heading}\n")).map(|at| at + 1).unwrap_or_else(|| panic!("no section {title:?}"));
    let body = &text[start + heading.len()..];
    let end = body.find("\n## ").unwrap_or(body.len());
    &body[..end]
}

/// `text` without the first case-insensitive occurrence of `phrase` (ASCII only, so the byte offsets agree).
fn without(text: &str, phrase: &str) -> String {
    let at = text.to_lowercase().find(&phrase.to_lowercase()).unwrap_or_else(|| panic!("{phrase:?} is not in the text"));
    format!("{}{}", &text[..at], &text[at + phrase.len()..])
}

/// The labels of the private planning tree that may not appear in a public file, as the plain-text shapes the project
/// scan looks for: a plan id of one of the phases, a threat id, a phase directory name, a decision id and a spike label.
/// Every pattern is built from pieces so that this file carries no label itself.
fn plan_labels(text: &str) -> Vec<String> {
    let mut hits = Vec::new();
    let phases = [("0", "7"), ("0", "8"), ("1", "0"), ("1", "1"), ("1", "3")];
    let bytes = text.as_bytes();
    for (a, b) in phases {
        let phase = format!("{a}{b}");
        // A plan id: the phase, a hyphen and two digits, not inside a longer number, a date or a word.
        let id = format!("{phase}-");
        let mut from = 0;
        while let Some(rel) = text[from..].find(&id) {
            let at = from + rel;
            let before = if at == 0 { b' ' } else { bytes[at - 1] };
            let after = &bytes[at + id.len()..];
            let two_digits = after.len() >= 2 && after[0].is_ascii_digit() && after[1].is_ascii_digit();
            let third = after.get(2).copied().unwrap_or(b' ');
            let standalone = !(before.is_ascii_alphanumeric() || before == b'.' || before == b'_' || before == b'-');
            if two_digits && standalone && !third.is_ascii_digit() {
                hits.push(text[at..at + id.len() + 2].to_owned());
            }
            from = at + id.len();
        }
        let threat = format!("T-{phase}-");
        if text.contains(&threat) {
            hits.push(threat);
        }
    }
    for directory in ["13-", "11-", "10-", "08-", "07-"].iter().map(|p| format!("{p}{}", "verteilung")) {
        if text.contains(&directory) {
            hits.push(directory);
        }
    }
    let spike = String::from("spike") + " S";
    for digit in '0'..='9' {
        if text.contains(&format!("{spike}{digit}")) {
            hits.push(format!("{spike}{digit}"));
        }
    }
    // A decision id: `D-` and two digits, standing alone.
    let mut from = 0;
    while let Some(rel) = text[from..].find("D-") {
        let at = from + rel;
        let before = if at == 0 { b' ' } else { bytes[at - 1] };
        let after = &bytes[at + 2..];
        if !before.is_ascii_alphanumeric()
            && after.len() >= 2
            && after[0].is_ascii_digit()
            && after[1].is_ascii_digit()
            && !after.get(2).copied().unwrap_or(b' ').is_ascii_digit()
        {
            hits.push(text[at..at + 4].to_owned());
        }
        from = at + 2;
    }
    hits
}

// ---- Task 1: the statement ------------------------------------------------------------------------------------

/// What the statement must say about itself and about what it does not do.
const CORE_CLAIMS: [&str; 10] = [
    "holzcloud.ch",
    "no account with the developer",
    "no telemetry",
    "no crash report",
    "no usage counter",
    "no update check",
    "https://github.com/holzcloud/holzWave/issues",
    "no store has accepted it",
    "Last updated:",
    "Opening the app, a chat, the providers dialog or a pane makes no request",
];

#[test]
fn the_statement_names_the_developer_says_what_it_does_not_do_and_where_to_ask() {
    assert_eq!(missing(PRIVACY, &CORE_CLAIMS), Vec::<&str>::new());
}

#[test]
fn the_matcher_fails_when_a_key_sentence_is_removed() {
    // The statement as it is passes; a copy without the telemetry phrase fails, and so do copies without the other claims.
    assert!(missing(PRIVACY, &CORE_CLAIMS).is_empty());
    let without_telemetry = without(PRIVACY, "no telemetry");
    assert_eq!(missing(&without_telemetry, &CORE_CLAIMS), ["no telemetry"]);
    let without_developer = PRIVACY.replace("holzcloud.ch", "the developer");
    assert!(missing(&without_developer, &CORE_CLAIMS).contains(&"holzcloud.ch"));
    // A phrase split by a line break still matches, and the case does not matter.
    assert!(missing("No\ntelemetry at all", &["no telemetry"]).is_empty());
    assert_eq!(missing("nothing here", &["no telemetry"]), ["no telemetry"]);
    // A section is cut at the next heading.
    assert_eq!(section("x\n## A\none\n## B\ntwo\n", "A").trim(), "one");
}

/// The four ways a connection is opened by the person: the words of the statement and the words of the Privacy section of
/// the configuration page. A change to either side fails the test until the other follows.
const FOUR_CASES: [(&str, &str); 4] = [
    ("an SSH connection you start", "an SSH connection you start"),
    ("the sync you set up", "the sync you set up"),
    ("a link you click", "a link you click"),
    ("a provider you set up", "set up a provider"),
];

/// Facts that both texts state, in the words each uses: (a phrase of the statement, a phrase of the configuration page).
const SHARED_FACTS: [(&str, &str); 7] = [
    ("no telemetry", "no telemetry"),
    ("no crash report", "no crash report"),
    ("no usage counter", "no usage counter"),
    ("no update check", "no update check"),
    ("`xdg-open` or `explorer`", "`xdg-open` or `explorer`"),
    ("`ai_local_only`", "`ai_local_only`"),
    ("OpenRouter not to collect it", "OpenRouter not to collect it"),
];

#[test]
fn the_statement_lists_the_four_connections_and_the_configuration_page_lists_the_same_four() {
    let privacy = section(CONFIG, "Privacy");
    for (statement, config) in FOUR_CASES {
        assert!(missing(PRIVACY, &[statement]).is_empty(), "PRIVACY.md lost the case {statement:?}");
        assert!(missing(privacy, &[config]).is_empty(), "the Privacy section of docs/config.md lost the case {config:?}");
    }
    for (statement, config) in SHARED_FACTS {
        assert!(missing(PRIVACY, &[statement]).is_empty(), "PRIVACY.md lost {statement:?}");
        assert!(missing(CONFIG, &[config]).is_empty(), "docs/config.md lost {config:?}");
    }
    // The statement has no fifth way: nothing else in it says a connection is opened without being asked for.
    assert!(missing(PRIVACY, &["only when you ask for one"]).is_empty());
}

#[test]
fn the_comparison_fails_when_one_side_drops_a_case() {
    let privacy = section(CONFIG, "Privacy");
    let trimmed = without(privacy, "an SSH connection you start");
    assert_eq!(missing(&trimmed, &[FOUR_CASES[0].1]), ["an SSH connection you start"]);
    let statement = without(PRIVACY, "a provider you set up");
    assert_eq!(missing(&statement, &[FOUR_CASES[3].0]), ["a provider you set up"]);
}

#[test]
fn the_statement_says_where_secrets_live_and_what_stays_on_the_computer() {
    assert_eq!(
        missing(
            PRIVACY,
            &[
                "Secrets are in the operating system keychain only",
                "never written to a file and never synced",
                "Chats and history stay on the computer",
                "They are not synced",
                "best effort and no guarantee",
            ],
        ),
        Vec::<&str>::new()
    );
    // The configuration page says the same about the keychain, the keys and the chats.
    assert_eq!(
        missing(
            CONFIG,
            &["Provider keys are in the operating system keychain only", "are never synced", "chats/", "best effort and no guarantee"],
        ),
        Vec::<&str>::new()
    );
}

#[test]
fn the_statement_is_scoped_to_what_exists() {
    assert_eq!(
        missing(
            PRIVACY,
            &[
                "covers the desktop builds and the server build",
                "The Android app is not published",
                "makes no claim about it",
                "Flathub",
                "the Microsoft Store",
                "F-Droid",
                "Obtainium",
                "has its own statement",
            ],
        ),
        Vec::<&str>::new()
    );
    // It claims nothing about a channel that exists today.
    let lowered = flat(PRIVACY).to_lowercase();
    for claim in ["available now", "now live", "published on flathub", "approved by"] {
        assert!(!lowered.contains(claim), "PRIVACY.md says {claim:?}");
    }
}

#[test]
fn the_statement_has_a_date_line_no_plan_label_and_no_non_ascii_letter() {
    let date = PRIVACY.lines().find_map(|line| line.strip_prefix("Last updated: ")).expect("a `Last updated:` line");
    let parts: Vec<&str> = date.trim().split('-').collect();
    assert!(
        parts.len() == 3
            && parts[0].len() == 4
            && parts[1].len() == 2
            && parts[2].len() == 2
            && parts.iter().all(|p| p.bytes().all(|b| b.is_ascii_digit())),
        "the date line is {date:?}, not YYYY-MM-DD"
    );
    assert_eq!(plan_labels(PRIVACY), Vec::<String>::new(), "labels of the private planning tree in PRIVACY.md");
    let odd: Vec<char> = PRIVACY.chars().filter(|c| !c.is_ascii()).collect();
    assert!(odd.is_empty(), "PRIVACY.md holds non-ASCII characters {odd:?}");
}

#[test]
fn the_label_scan_sees_a_label_and_leaves_dates_and_versions_alone() {
    let id = String::from("13") + "-" + "14";
    let threat = String::from("T-1") + "3-041";
    let decision = String::from("D") + "-10";
    assert_eq!(plan_labels(&format!("see {id}")), [id.clone()]);
    assert_eq!(plan_labels(&format!("({id}) ok")), [id]);
    assert_eq!(plan_labels(&format!("a bound {threat}")), [String::from("T-1") + "3-"]);
    assert_eq!(plan_labels(&format!("as {decision} says")), [decision]);
    assert!(plan_labels("on 2026-11-06 and v0.0.1-beta2 and 1.13-14x").is_empty());
}

// ---- Task 2: the disclosure and the links in the README ---------------------------------------------------------

const README: &str = include_str!("../../README.md");

/// The text of the README section whose heading holds `AI` and `built with AI` (any case), up to the next `## ` heading.
fn disclosure(readme: &str) -> &str {
    let start = readme
        .lines()
        .scan(0usize, |offset, line| {
            let at = *offset;
            *offset += line.len() + 1;
            Some((at, line))
        })
        .find(|(_, line)| line.starts_with("## ") && line.to_lowercase().contains("built with ai"))
        .map(|(at, _)| at)
        .expect("the README has no section `Built with AI help`");
    let body = &readme[start..];
    let end = body[3..].find("\n## ").map_or(body.len(), |at| at + 3);
    &body[..end]
}

/// The words German text would add to an English file; a few that no English sentence of this project uses.
const GERMAN_WORDS: [&str; 8] = [" und ", " nicht ", " der ", " die ", " das ", " ist ", " wird ", " für "];

#[test]
fn the_readme_discloses_the_ai_help_with_the_parts_the_extent_and_the_owners_part() {
    let text = disclosure(README);
    assert_eq!(
        missing(
            text,
            &[
                "AI coding assistant",
                "Claude, made by Anthropic",
                "the code",
                "the tests",
                "the documentation",
                "the packaging files",
                "commits",
                "Co-Authored-By: Claude",
                "The owner reads the results, runs the tests",
                "decides what ships",
            ],
        ),
        Vec::<&str>::new()
    );
    // The extent is a number from the history, not an adjective alone: two counts and the day they were taken.
    let digits = text.split(|c: char| !c.is_ascii_digit() && c != ',').filter(|w| w.chars().filter(char::is_ascii_digit).count() >= 3);
    assert!(digits.count() >= 2, "the disclosure names no commit counts");
    assert_eq!(missing(text, &["on 2026-"]), Vec::<&str>::new(), "the counts carry no date");
}

#[test]
fn the_readme_says_the_owner_makes_the_flathub_submission_by_hand() {
    let text = disclosure(README);
    assert_eq!(
        missing(
            text,
            &[
                "Flathub manifest",
                "the submission",
                "written and made by the owner by hand",
                "`packaging/flatpak/`",
                "references",
                "not a submission",
            ],
        ),
        Vec::<&str>::new()
    );
}

#[test]
fn the_readme_links_the_statement_the_distribution_page_and_the_issues() {
    assert_eq!(
        missing(
            README,
            &[
                "[Privacy statement](PRIVACY.md)",
                "[Distribution channels](docs/distribution.md)",
                "[GitHub Issues](https://github.com/holzcloud/holzWave/issues)",
                "Questions and bug reports go to",
            ],
        ),
        Vec::<&str>::new()
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("the workspace root");
    assert!(root.join("PRIVACY.md").is_file() && root.join("docs/distribution.md").is_file(), "a linked file is missing");
}

#[test]
fn the_readme_has_no_planning_label_and_no_german_and_the_disclosure_matcher_can_fail() {
    assert_eq!(plan_labels(README), Vec::<String>::new(), "labels of the private planning tree in README.md");
    let lowered = format!(" {} ", flat(README).to_lowercase());
    for word in GERMAN_WORDS {
        assert!(!lowered.contains(word), "README.md holds the German word {word:?}");
    }
    assert!(!README.chars().any(|c| "äöüßÄÖÜ".contains(c)), "README.md holds a German letter");
    // The matcher fails on a disclosure without the owner's part, and on a README without the section.
    let cut = without(disclosure(README), "decides what ships");
    assert_eq!(missing(&cut, &["decides what ships"]), ["decides what ships"]);
    let result = std::panic::catch_unwind(|| disclosure("# Title\n\n## Other\ntext\n").len());
    assert!(result.is_err(), "a README without the section is accepted");
}
