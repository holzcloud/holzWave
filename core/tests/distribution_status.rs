//! The public texts about distribution agree with each other and say only what is true.
//!
//! `docs/distribution.md` is the one page that gives every channel a status, in a closed set of words. The README, the
//! status page and the signing page must not say more than it does. These tests read the files as text (std only, no
//! crate). Every matcher has an in-memory self-test, so a matcher that stopped matching would fail here instead of
//! letting a wrong claim through.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const PAGE: &str = include_str!("../../docs/distribution.md");
const README: &str = include_str!("../../README.md");
const STATUS_PAGE: &str = include_str!("../../docs/status.md");
const SIGNING: &str = include_str!("../../docs/signing.md");

/// The only words a channel's status may be.
const STATUS_WORDS: [&str; 6] = ["not built", "not submitted", "in review", "available", "on hold", "refused"];

/// The channels of the page, by the text its table row starts with after the first bold mark.
const CHANNELS: [&str; 6] = ["GitHub downloads", "Homebrew", "Flathub", "Microsoft Store", "Obtainium", "F-Droid"];

/// For each channel, the lower-case text that names it as a way to install in the README.
const ROUTE_KEYS: [(&str, &[&str]); 6] = [
    ("GitHub downloads", &["github.com/holzcloud/holzwave/releases"]),
    ("Homebrew", &["brew "]),
    ("Flathub", &["flathub", "flatpak install", "flatpak remote"]),
    ("Microsoft Store", &["microsoft store", "apps.microsoft.com", "ms-windows-store"]),
    ("Obtainium", &["obtainium"]),
    ("F-Droid", &["f-droid", "fdroid"]),
];

/// The text with every run of white space made one blank.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text of the `## <title>` section, up to the next `## ` heading or the end.
fn section<'a>(text: &'a str, title: &str) -> &'a str {
    let heading = format!("## {title}");
    let start = if text.starts_with(&format!("{heading}\n")) {
        0
    } else {
        text.find(&format!("\n{heading}\n")).map(|at| at + 1).unwrap_or_else(|| panic!("no section {title:?}"))
    };
    let body = &text[start + heading.len()..];
    let end = body.find("\n## ").unwrap_or(body.len());
    &body[..end]
}

/// The words between backticks in `cell`.
fn code_spans(cell: &str) -> Vec<&str> {
    cell.split('`').skip(1).step_by(2).collect()
}

/// The table rows of the channel table: the row's name and its status cell. A row whose status cell does not start with a
/// backticked word of the closed set, or that holds a second such word, is reported by name in the second list.
fn channel_statuses(page: &str) -> (Vec<(String, String)>, Vec<String>) {
    let mut found = Vec::new();
    let mut problems = Vec::new();
    for line in page.lines().filter(|l| l.starts_with("| **")) {
        let name = line[4..].split("**").next().unwrap_or("").to_owned();
        let cells: Vec<&str> = line.trim_matches('|').split('|').collect();
        let Some(cell) = cells.get(1).map(|c| c.trim()) else {
            problems.push(format!("{name}: the row has no status cell"));
            continue;
        };
        let words: Vec<&str> = code_spans(cell).into_iter().filter(|w| STATUS_WORDS.contains(w)).collect();
        let first = cell.strip_prefix('`').and_then(|rest| rest.split('`').next());
        match first {
            Some(word) if STATUS_WORDS.contains(&word) && words.len() == 1 => found.push((name, word.to_owned())),
            Some(word) if !STATUS_WORDS.contains(&word) => problems.push(format!("{name}: `{word}` is not a status word")),
            Some(_) => problems.push(format!("{name}: the status cell holds {} status words, not one", words.len())),
            None => problems.push(format!("{name}: the status cell does not start with a status word")),
        }
    }
    (found, problems)
}

/// The status of the channel that starts with `channel`, if the table has one.
fn status_of<'a>(statuses: &'a [(String, String)], channel: &str) -> Option<&'a str> {
    statuses.iter().find(|(name, _)| name.starts_with(channel)).map(|(_, word)| word.as_str())
}

/// The channels `readme` names as a way to install: in a heading, inside a code fence, anywhere in an `Install` section, or
/// in the text or address of a link to a web address. A mention in running prose elsewhere is not a route.
fn readme_routes(readme: &str) -> BTreeSet<&'static str> {
    let mut routes = BTreeSet::new();
    let mut fenced = false;
    let mut install = false;
    for line in readme.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced && line.starts_with("## ") {
            install = line.to_lowercase().starts_with("## install");
        }
        let lowered = line.to_lowercase();
        let heading = !fenced && line.starts_with('#');
        for (channel, keys) in ROUTE_KEYS {
            let named = keys.iter().any(|k| lowered.contains(k));
            if named && (fenced || heading || install) {
                routes.insert(channel);
            }
            if !fenced && web_links(line).iter().any(|(text, target)| keys.iter().any(|k| text.to_lowercase().contains(k) || target.to_lowercase().contains(k))) {
                routes.insert(channel);
            }
        }
    }
    routes
}

/// The `(text, target)` of each Markdown link on `line` whose target is a web address.
fn web_links(line: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = line[from..].find("](") {
        let close = from + rel;
        let open = line[..close].rfind('[').map_or(0, |at| at + 1);
        let tail = &line[close + 2..];
        let Some(end) = tail.find(')') else { break };
        let target = &tail[..end];
        if target.contains("://") {
            out.push((line[open..close].to_owned(), target.to_owned()));
        }
        from = close + 2 + end;
    }
    out
}

/// The channels the README names as a way to install while the page does not call them `available`.
fn route_violations(readme: &str, page: &str) -> Vec<String> {
    let (statuses, _) = channel_statuses(page);
    readme_routes(readme)
        .into_iter()
        .filter_map(|channel| match status_of(&statuses, channel) {
            Some("available") => None,
            Some(other) => Some(format!("the README names {channel} as a way to install, and the page says `{other}`")),
            None => Some(format!("the README names {channel} as a way to install, and the page has no row for it")),
        })
        .collect()
}

/// A small page with the channel table's rows in the page's own shape.
fn page_with(rows: &[(&str, &str)]) -> String {
    let mut page = String::from("| Channel | Status today | Cost |\n|---|---|---|\n");
    for (name, status) in rows {
        page.push_str(&format!("| **{name}** (more) | `{status}`. Some text | Nothing |\n"));
    }
    page
}

/// The Android channels that the page calls `available` while it says that the Android app is not built.
fn android_violations(page: &str) -> Vec<String> {
    if !flat(page).contains("The Android app is not built") {
        return vec!["the page does not say that the Android app is not built".to_owned()];
    }
    let (statuses, _) = channel_statuses(page);
    ["Obtainium", "F-Droid"]
        .into_iter()
        .filter(|c| status_of(&statuses, c) == Some("available"))
        .map(|c| format!("{c} is `available` while the Android app is not built"))
        .collect()
}

/// The GitHub slug of a heading.
fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// The links of `text` that do not resolve: a file link whose target `exists` says is missing, and a bare `#anchor` that no
/// heading of `text` has. Links with a scheme and mail links are not checked.
fn broken_links(text: &str, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let anchors: BTreeSet<String> = text.lines().filter(|l| l.starts_with('#')).map(|l| slug(l.trim_start_matches('#'))).collect();
    let mut broken = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("](") {
        rest = &rest[at + 2..];
        let Some(end) = rest.find(')') else { break };
        let target = rest[..end].to_owned();
        rest = &rest[end..];
        if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
            continue;
        }
        if let Some(anchor) = target.strip_prefix('#') {
            if !anchors.contains(anchor) {
                broken.push(format!("no heading for {target}"));
            }
            continue;
        }
        let file = target.split('#').next().unwrap_or("");
        if !exists(file) {
            broken.push(format!("{target} does not exist"));
        }
    }
    broken
}

/// The names that `untested_section` has to carry, each in a bullet that states a limit or a recorded result.
const UNTESTED_NAMES: [&str; 6] = ["Linux desktops", "Linux aarch64", "Windows ARM64", "Flatpak", "MSIX", "Android"];

/// The names of `UNTESTED_NAMES` that no bullet of the section carries together with a limit (`not`, `never`, `none`,
/// `nothing`, `no`) or a recorded `pass` or `fail`.
fn untested_gaps(section_text: &str) -> Vec<&'static str> {
    let mut bullets: Vec<String> = Vec::new();
    for line in section_text.lines() {
        if line.starts_with("- ") {
            bullets.push(line.to_owned());
        } else if let Some(last) = bullets.last_mut() {
            last.push(' ');
            last.push_str(line.trim());
        }
    }
    UNTESTED_NAMES
        .into_iter()
        .filter(|name| {
            !bullets.iter().any(|b| {
                let words: Vec<String> = b.to_lowercase().split(|c: char| !c.is_alphanumeric()).map(str::to_owned).collect();
                let limit = ["not", "never", "none", "nothing", "no", "pass", "fail"].iter().any(|w| words.iter().any(|x| x == w));
                b.to_lowercase().contains(&name.to_lowercase()) && limit
            })
        })
        .collect()
}

/// The lines `- **<channel>:** `<word>`` of the status page's `Distribution` section, as (channel, word).
fn status_page_lines(text: &str) -> Vec<(String, String)> {
    section(text, "Distribution")
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("- **")?;
            let (name, tail) = rest.split_once(":**")?;
            let word = tail.trim().strip_prefix('`')?.split('`').next()?;
            Some((name.to_owned(), word.to_owned()))
        })
        .collect()
}

/// The channels whose word on the status page differs from the distribution page, or that the status page lacks.
fn status_page_disagreements(status_page: &str, page: &str) -> Vec<String> {
    let (statuses, _) = channel_statuses(page);
    let lines = status_page_lines(status_page);
    let mut out = Vec::new();
    for channel in CHANNELS {
        let theirs = lines.iter().find(|(name, _)| name.starts_with(channel)).map(|(_, w)| w.as_str());
        let ours = status_of(&statuses, channel);
        if theirs != ours || ours.is_none() {
            out.push(format!("{channel}: the status page says {theirs:?}, the distribution page says {ours:?}"));
        }
    }
    out
}

/// Whether `text` says that no release has been signed.
fn says_unsigned(text: &str) -> bool {
    let lowered = flat(text).to_lowercase();
    lowered.contains("no release has been signed") || lowered.contains("none of the releases made so far carries")
}

/// The first block quote of the page (its status box).
fn status_box(text: &str) -> String {
    text.lines().skip_while(|l| !l.starts_with('>')).take_while(|l| l.starts_with('>')).collect::<Vec<_>>().join("\n")
}

fn docs_has(file: &str) -> bool {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("docs").join(file).exists()
}

fn root_has(file: &str) -> bool {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(file).exists()
}

#[test]
fn every_channel_has_one_status_word_from_the_closed_set() {
    // The matcher first, on strings.
    let ok = page_with(&[("Flathub", "not submitted"), ("Obtainium", "not built")]);
    let (found, problems) = channel_statuses(&ok);
    assert_eq!(found, [("Flathub".to_owned(), "not submitted".to_owned()), ("Obtainium".to_owned(), "not built".to_owned())]);
    assert!(problems.is_empty());
    let (_, outside) = channel_statuses(&page_with(&[("Flathub", "live")]));
    assert_eq!(outside.len(), 1);
    assert!(outside[0].starts_with("Flathub:") && outside[0].contains("live"), "{outside:?}");
    let (_, two) = channel_statuses("| **F-Droid** | `not built`, `not submitted`. text | x |\n");
    assert!(two.len() == 1 && two[0].starts_with("F-Droid:") && two[0].contains("2 status words"), "{two:?}");
    let (_, none) = channel_statuses("| **Homebrew** | Exists, not built | x |\n");
    assert!(none.len() == 1 && none[0].starts_with("Homebrew:"), "{none:?}");

    // Then the page.
    let (found, problems) = channel_statuses(PAGE);
    assert!(problems.is_empty(), "the status cell of a channel row is wrong:\n{}", problems.join("\n"));
    for channel in CHANNELS {
        assert!(status_of(&found, channel).is_some(), "the page has no row for {channel}");
    }
    assert_eq!(found.len(), CHANNELS.len(), "the page has a row that is not one of the six channels: {found:?}");
    // The words are defined once, in a table of the page.
    let flat_page = flat(PAGE);
    for word in STATUS_WORDS {
        assert!(flat_page.contains(&format!("| `{word}` |")), "the page does not define the status word `{word}`");
    }
}

#[test]
fn the_readme_names_a_channel_as_a_way_to_install_only_when_the_page_says_available() {
    let available = page_with(&[("Flathub", "available"), ("Homebrew", "available")]);
    let held = page_with(&[("Flathub", "not submitted"), ("Homebrew", "available")]);

    // The matcher first, on strings: each shape of a route is found, and running prose is not one.
    let routes = |text: &str| readme_routes(text).into_iter().collect::<Vec<_>>();
    assert_eq!(routes("## Install\n\n- Get it from Flathub.\n"), ["Flathub"]);
    assert_eq!(routes("## Other\n\n### Flathub\n"), ["Flathub"]);
    assert_eq!(routes("## Other\n\n```sh\nflatpak install flathub ch.holzcloud.holzwave\n```\n"), ["Flathub"]);
    assert_eq!(routes("## Other\n\nGet it [here](https://flathub.org/apps/x).\n"), ["Flathub"]);
    assert_eq!(routes("## Other\n\n[Obtainium](https://obtainium.imranr.dev)\n"), ["Obtainium"]);
    assert_eq!(routes("## Install\n\nOn the Microsoft Store.\n"), ["Microsoft Store"]);
    assert_eq!(routes("## Install\n\nSearch F-Droid.\n"), ["F-Droid"]);
    assert_eq!(routes("## Install\n\n```sh\nbrew install --cask x\n```\n"), ["Homebrew"]);
    assert_eq!(routes("## Install\n\n[releases](https://github.com/holzcloud/holzWave/releases)\n"), ["GitHub downloads"]);
    assert!(routes("## Built with AI help\n\nThe Flathub manifest is written by hand. See [the page](docs/distribution.md).\n").is_empty());
    assert!(routes("## Install\n\nA [guide](docs/flathub-submission.md) and nothing else.\n").len() == 1, "an Install section counts any mention");
    assert!(routes("## Other\n\nSee [the Flathub checklist](docs/flathub-submission.md).\n").is_empty(), "a link to a file is not a route");

    // A pass and a failure.
    assert!(route_violations("## Install\n\n- Get it from Flathub.\n", &available).is_empty());
    let failures = route_violations("## Install\n\n- Get it from Flathub.\n", &held);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].contains("Flathub") && failures[0].contains("not submitted"), "{failures:?}");
    assert_eq!(route_violations("## Install\n\n- Flathub\n", &page_with(&[("Homebrew", "available")])).len(), 1, "no row is a failure too");
    assert!(route_violations("## Built with AI help\n\nThe Flathub manifest is written by hand.\n", &held).is_empty());

    // The real texts.
    let real = readme_routes(README);
    assert!(real.contains("GitHub downloads") && real.contains("Homebrew"), "the matcher no longer sees the README's own routes: {real:?}");
    let failures = route_violations(README, PAGE);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn no_android_channel_is_available_while_the_app_is_not_built() {
    let built = "The Android app is not built.";
    let ok = format!("{}\n{built}\n", page_with(&[("Obtainium", "not built"), ("F-Droid", "not built")]));
    assert!(android_violations(&ok).is_empty());
    let bad = format!("{}\n{built}\n", page_with(&[("Obtainium", "available"), ("F-Droid", "not built")]));
    assert_eq!(android_violations(&bad), ["Obtainium is `available` while the Android app is not built"]);
    let silent = page_with(&[("Obtainium", "not built"), ("F-Droid", "not built")]);
    assert_eq!(android_violations(&silent).len(), 1, "a page that stops saying the app is missing fails");

    let violations = android_violations(PAGE);
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn every_link_of_the_distribution_page_and_the_readme_resolves() {
    // The matcher first, on strings.
    let text = "# Top\n## The channels\n[a](a.md), [b](b.md#x), [c](https://x.y/z), [d](#top), [e](#the-channels), [f](#nowhere), [g](mailto:a@b.c)";
    let present = |f: &str| f == "a.md";
    assert_eq!(broken_links(text, &present), ["b.md#x does not exist", "no heading for #nowhere"]);
    assert_eq!(slug("What would cost money, and is not planned"), "what-would-cost-money-and-is-not-planned");
    assert_eq!(slug("Flathub"), "flathub");

    // Then the files, with no exception for any page.
    let broken = broken_links(PAGE, &docs_has);
    assert!(broken.is_empty(), "docs/distribution.md: {}", broken.join("; "));
    let broken = broken_links(README, &root_has);
    assert!(broken.is_empty(), "README.md: {}", broken.join("; "));
    assert!(PAGE.contains("](homebrew.md)"), "the page lost its link to the Homebrew page");
    assert!(docs_has("homebrew.md"));
}

#[test]
fn the_untested_list_names_every_build_that_was_never_run() {
    let full = "- **Linux desktops:** none was started.\n- **Linux aarch64:** never run.\n- **Windows ARM64:** was not run.\n- **The Flatpak:** never started.\n- **The MSIX:** no package exists.\n- **Android:** nothing exists.\n";
    assert!(untested_gaps(full).is_empty());
    // A result that was recorded may replace the limit.
    assert!(untested_gaps(&full.replace("**Android:** nothing exists.", "**Android:** row AND1 holds pass.")).is_empty());
    // A name that is missing, or named without a limit, is found.
    assert_eq!(untested_gaps(&full.replace("- **The MSIX:** no package exists.\n", "")), ["MSIX"]);
    assert_eq!(untested_gaps(&full.replace("**Android:** nothing exists.", "**Android:** planned.")), ["Android"]);
    assert_eq!(untested_gaps(&full.replace("**Linux aarch64:** never run.\n", "**Linux aarch64:** never run.\n").replace("Linux desktops", "Desktops")), ["Linux desktops"]);
    // A bullet that wraps keeps its limit.
    assert!(untested_gaps("- **Linux desktops:**\n  GNOME and the rest.\n  None was started.\n").len() == 5);

    let gaps = untested_gaps(section(PAGE, "Untested"));
    assert!(gaps.is_empty(), "the Untested list lacks a limit statement for: {gaps:?}");
}

#[test]
fn the_status_page_lists_the_same_words_and_the_same_untested_builds() {
    let page = page_with(&[
        ("GitHub downloads", "available"),
        ("Homebrew", "available"),
        ("Flathub", "not submitted"),
        ("Microsoft Store", "on hold"),
        ("Obtainium", "not built"),
        ("F-Droid", "not built"),
    ]);
    let same = "# Status\n\n## Distribution\n\n- **GitHub downloads:** `available`\n- **Homebrew:** `available`\n- **Flathub:** `not submitted`\n- **Microsoft Store:** `on hold`\n- **Obtainium:** `not built`\n- **F-Droid:** `not built`\n\n## Stack\n";
    assert!(status_page_disagreements(same, &page).is_empty());
    let differs = same.replace("**Flathub:** `not submitted`", "**Flathub:** `in review`");
    assert_eq!(status_page_disagreements(&differs, &page).len(), 1);
    let missing = same.replace("- **F-Droid:** `not built`\n", "");
    assert_eq!(status_page_disagreements(&missing, &page).len(), 1);

    let disagreements = status_page_disagreements(STATUS_PAGE, PAGE);
    assert!(disagreements.is_empty(), "{}", disagreements.join("\n"));
    let text = flat(section(STATUS_PAGE, "Distribution")).to_lowercase();
    for name in ["linux desktops", "linux aarch64", "windows arm64", "flatpak", "microsoft store package", "android", "every release is a beta"] {
        assert!(text.contains(name), "the Distribution section of docs/status.md does not name {name:?}");
    }
}

#[test]
fn the_readme_and_the_signing_page_say_the_same_about_signed_releases() {
    assert!(says_unsigned("> **Status.** No release has been signed yet."));
    assert!(says_unsigned("none of the\nreleases made so far carries one"));
    assert!(!says_unsigned("The release 0.0.1-beta3 was signed."));
    assert_eq!(status_box("# T\n\n> one\n> two\n\nafter\n> later\n"), "> one\n> two");

    let signing = status_box(SIGNING);
    assert!(!signing.is_empty(), "docs/signing.md lost its status box");
    assert_eq!(
        says_unsigned(&signing),
        says_unsigned(README),
        "the status box of docs/signing.md and the README disagree on whether any release has been signed"
    );
    // Whichever way it is, the box names the version it speaks about.
    assert!(flat(&signing).contains("0.0.1-beta"), "the status box does not name the release it speaks about");
}

#[test]
fn the_files_are_where_the_tests_expect_them() {
    let root: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for file in ["docs/distribution.md", "README.md", "docs/status.md", "docs/signing.md", "docs/homebrew.md"] {
        assert!(root.join(file).is_file(), "{file} is missing");
    }
}
