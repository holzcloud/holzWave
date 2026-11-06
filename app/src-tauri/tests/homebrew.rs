//! The Homebrew channel tells the truth: the cask the release script writes and the cask that is committed carry a
//! `caveats` stanza that says what the cask does to the Mac (an ad hoc signature, no Apple notarization, the removal of
//! the quarantine attribute, who publishes it), and the release notes say the same. These are text tests: the script
//! is shell and the cask is Ruby, so the facts are pinned by phrase, and whitespace is squashed first so a line wrap
//! cannot hide a phrase. `ruby -c Casks/holzwave@preview.rb` is the syntax check and runs outside this file.

const RELEASE_SCRIPT: &str = include_str!("../../../scripts/release-macos.sh");
const PREVIEW_CASK: &str = include_str!("../../../Casks/holzwave@preview.rb");

/// Collapse every run of whitespace to one space.
fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The squashed text of the `caveats <<~EOS ... EOS` block of a cask (or of the script's template).
fn caveats(source: &str) -> String {
    let start = source.find("caveats <<~EOS").expect("the source has a caveats stanza");
    let body = &source[start + "caveats <<~EOS".len()..];
    let end = body.find("\n  EOS").expect("the caveats stanza is closed");
    squash(&body[..end])
}

/// The caveats of the cask template that `write_cask` writes, without the shell variable that carries the optional
/// pre-release sentence.
fn template_caveats() -> String {
    caveats(RELEASE_SCRIPT).replace("$pre_note", "")
}

/// The caveats of the committed cask, without the pre-release sentence.
fn cask_caveats() -> String {
    caveats(PREVIEW_CASK).replace(PRE_RELEASE, "")
}

const PRE_RELEASE: &str = "This is a pre-release.";

/// The four facts, in the order the text tells them.
fn assert_four_facts(text: &str, whose: &str) {
    let ad_hoc = text.find("signed ad hoc").unwrap_or_else(|| panic!("{whose} does not say the app is signed ad hoc"));
    let notarized = text
        .find("not notarized by Apple")
        .unwrap_or_else(|| panic!("{whose} does not say the app is not notarized by Apple"));
    let quarantine = text
        .find("removes the quarantine attribute")
        .unwrap_or_else(|| panic!("{whose} does not say the cask removes the quarantine attribute"));
    let bypass = text
        .find("bypasses a protection of macOS")
        .unwrap_or_else(|| panic!("{whose} does not call the removal a bypass of a macOS protection"));
    let publisher = text
        .find("holzcloud.ch")
        .unwrap_or_else(|| panic!("{whose} does not name the publisher holzcloud.ch"));
    assert!(ad_hoc < notarized, "{whose}: ad hoc comes before not notarized");
    assert!(notarized < quarantine, "{whose}: not notarized comes before the quarantine removal");
    assert!(quarantine < bypass, "{whose}: the removal comes before the word bypass");
    assert!(bypass < publisher, "{whose}: the publisher comes last");
}

#[test]
fn the_template_and_the_committed_cask_have_the_four_facts() {
    assert_four_facts(&template_caveats(), "the cask template of scripts/release-macos.sh");
    assert_four_facts(&cask_caveats(), "Casks/holzwave@preview.rb");
}

#[test]
fn the_template_and_the_committed_cask_say_the_same() {
    assert_eq!(
        squash(&template_caveats()),
        squash(&cask_caveats()),
        "the caveats of the template and of the committed cask differ"
    );
}

#[test]
fn the_pre_release_sentence_depends_on_a_hyphen_in_the_version() {
    let script = squash(RELEASE_SCRIPT);
    // The sentence is produced by a `case` on the version, the way the stable cask is decided, and only for a hyphen.
    assert!(
        script.contains(&format!("case \"$version\" in *-*) pre_note=")),
        "the template does not set the pre-release note for a version with a hyphen"
    );
    assert!(script.contains(PRE_RELEASE), "the script does not carry the pre-release sentence");
    // The note is empty first, so a plain version gets no sentence.
    assert!(script.contains("pre_note="), "pre_note is not initialised");
    let init = script.find("pre_note= ").expect("pre_note is initialised empty before the case");
    let set = script.find("*-*) pre_note=").expect("pre_note is set in a case");
    assert!(init < set, "pre_note must be emptied before the case sets it");
    // The committed cask is of a version with a hyphen (0.0.1-beta2 at the time of writing), so it carries the sentence.
    let version = PREVIEW_CASK
        .lines()
        .find_map(|line| line.trim().strip_prefix("version \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .expect("the cask has a version");
    assert_eq!(
        caveats(PREVIEW_CASK).contains(PRE_RELEASE),
        version.contains('-'),
        "the committed cask {version} and its pre-release sentence disagree"
    );
}

#[test]
fn the_release_notes_say_the_same_facts() {
    let script = squash(RELEASE_SCRIPT);
    assert!(script.contains("the app is signed ad hoc only"), "the release notes do not say ad hoc");
    assert!(script.contains("is not notarized by Apple"), "the release notes do not say not notarized");
    assert!(script.contains("the cask removes the quarantine attribute"), "the release notes lost the quarantine sentence");
    assert!(script.contains("Published by holzcloud.ch"), "the release notes do not name the publisher");
}

#[test]
fn the_committed_cask_keeps_its_release_fields() {
    // This channel text is not a release: the fields that make one stay as they are, and the structured postflight stays.
    assert!(PREVIEW_CASK.contains("postflight_steps do"), "the quarantine step is no longer a structured step");
    assert!(PREVIEW_CASK.contains("depends_on arch: :arm64"), "the cask no longer depends on arm64");
    assert!(PREVIEW_CASK.contains("conflicts_with cask: \"holzwave\""), "the preview cask lost its conflict");
}

const HOMEBREW_PAGE: &str = include_str!("../../../docs/homebrew.md");

#[test]
fn the_page_says_why_the_official_repository_is_out_of_reach() {
    let page = squash(HOMEBREW_PAGE);
    for phrase in [
        "Homebrew/homebrew-cask",
        "holzcloud.ch",
        "2026-11-05",
        "not notarized",
        "pre-release",
        "2026-09-01",
        "Notability",
        "3 problems in 1 cask detected",
        "GitHub repository not notable enough",
        "Intel Macs",
        "has not been made by anyone",
    ] {
        assert!(page.contains(phrase), "docs/homebrew.md does not contain {phrase:?}");
    }
    assert!(
        page.contains("No pull request to `Homebrew/homebrew-cask` is opened"),
        "the page does not say that no pull request is opened"
    );
}

#[test]
fn the_page_does_not_state_the_apple_price_as_fact() {
    let page = squash(HOMEBREW_PAGE);
    let at = page.find("$99").expect("the page names the price of the Apple program");
    let around = &page[at.saturating_sub(40)..(at + 60).min(page.len())];
    assert!(around.contains("about $99"), "the price must be written as about $99: {around:?}");
    assert!(around.contains("as far as the author knows"), "the price must be marked as the author's belief: {around:?}");
}

#[test]
fn the_page_carries_no_label_of_the_planning_tree() {
    for (name, text) in [("docs/homebrew.md", HOMEBREW_PAGE), ("Casks/holzwave@preview.rb", PREVIEW_CASK), ("scripts/release-macos.sh", RELEASE_SCRIPT)] {
        assert!(!text.contains("T-13-"), "{name} carries a threat id");
        assert!(!text.contains("13-verteilung"), "{name} carries a phase directory name");
        assert!(!text.contains("D-18") && !text.contains("D-13"), "{name} carries a decision id");
    }
}
