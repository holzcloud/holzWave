//! Names that come from a cluster, an account, a directory or a synced file are another party's text. This file is the
//! corpus that proves none of them can become an option of a tool, break a terminal or be echoed back by a refusal.
//!
//! The inputs with special characters are built from code points (`ch`), never typed, so that the file itself holds no
//! hidden character. A refusal is judged twice: by its outcome and by its text, which has to be one of a fixed list of
//! sentences and must not contain the input.

use holzwave_core::integrate::args::{flag_eq, ArgError, ContextName};

/// A scalar value from its code point.
fn ch(code: u32) -> char {
    char::from_u32(code).expect("a scalar value")
}

/// Every sentence a refusal can be. A refusal text outside this list, or one that holds the input, is a defect.
const SENTENCES: &[&str] = &[
    "the value is empty",
    "the value is longer than allowed",
    "the value starts with a dash",
    "the value holds a control or invisible character",
    "the value has the wrong form for its kind",
];

/// The refusal is one fixed sentence and holds none of the input.
fn assert_fixed_sentence(error: ArgError, input: &str) {
    let text = error.to_string();
    assert!(SENTENCES.contains(&text.as_str()), "a refusal text outside the fixed list: {text:?}");
    if input.chars().count() > 3 {
        assert!(!text.contains(input), "a refusal text that holds its input (input of {} bytes)", input.len());
    }
}

/// `(input, accepted)`: every row says what a context name has to do.
fn context_corpus() -> Vec<(String, bool)> {
    let hidden = |code: u32| format!("a{}b", ch(code));
    vec![
        ("dev".into(), true),
        ("kind-test".into(), true),
        ("arn:aws:eks:eu-central-1:000000000000:cluster/prod".into(), true),
        ("gke_proj_europe-west1_cluster-1".into(), true),
        ("user@cluster".into(), true),
        ("a b".into(), true),
        ("a=b".into(), true),
        ("-".into(), false),
        ("a".repeat(253), true),
        ("".into(), false),
        ("-x".into(), false),
        ("--server=https://x".into(), false),
        ("--context=x".into(), false),
        ("-".repeat(2), false),
        (" a".into(), false),
        ("a ".into(), false),
        ("a\0b".into(), false),
        ("a\nb".into(), false),
        ("a\tb".into(), false),
        ("a\rb".into(), false),
        (hidden(0x1b), false),
        (hidden(0x7f), false),
        (hidden(0x202e), false),
        (hidden(0x2066), false),
        (hidden(0x200b), false),
        (hidden(0xfeff), false),
        (hidden(0xad), false),
        (hidden(0xe0041), false),
        (hidden(0xa0), false),
        (hidden(0x2003), false),
        ("a".repeat(254), false),
    ]
}

#[test]
fn a_hostile_context_name_never_becomes_an_option() {
    for (index, (input, accepted)) in context_corpus().into_iter().enumerate() {
        match (ContextName::parse(&input), accepted) {
            (Ok(name), true) => {
                assert_eq!(name.as_str(), input, "row {index}: the name is kept as given");
                let flag = flag_eq("context", name.as_str());
                let expected = format!("--context={input}");
                assert_eq!(flag, std::ffi::OsString::from(&expected), "row {index}: one argument in the equals form");
                assert!(
                    flag.to_string_lossy().starts_with("--context="),
                    "row {index}: an accepted name cannot change the start of the argument"
                );
            }
            (Err(error), false) => assert_fixed_sentence(error, &input),
            (Ok(_), false) => panic!("row {index}: a name that must be refused was accepted (input of {} bytes)", input.len()),
            (Err(error), true) => panic!("row {index}: a name that must be accepted was refused: {error}"),
        }
    }
}

#[test]
fn a_name_with_an_equals_sign_stays_one_argument() {
    let name = ContextName::parse("a=b").expect("an equals sign is a valid character of a context");
    let flag = flag_eq("context", name.as_str());
    assert_eq!(flag, std::ffi::OsString::from("--context=a=b"));
    let spaced = ContextName::parse("a b").expect("a space inside a context is valid");
    assert_eq!(flag_eq("context", spaced.as_str()), std::ffi::OsString::from("--context=a b"));
}

#[test]
fn the_refusal_reasons_are_told_apart() {
    assert_eq!(ContextName::parse("").unwrap_err(), ArgError::Empty);
    assert_eq!(ContextName::parse("-x").unwrap_err(), ArgError::LeadingDash);
    assert_eq!(ContextName::parse(&"a".repeat(254)).unwrap_err(), ArgError::TooLong);
    assert_eq!(ContextName::parse("a\0b").unwrap_err(), ArgError::Hidden);
    assert_eq!(ContextName::parse(" a").unwrap_err(), ArgError::Shape);
    assert_eq!(ContextName::parse("a ").unwrap_err(), ArgError::Shape);
}

#[test]
fn every_refusal_text_is_one_fixed_sentence() {
    let all = [ArgError::Empty, ArgError::TooLong, ArgError::LeadingDash, ArgError::Hidden, ArgError::Shape];
    let texts: Vec<String> = all.iter().map(ToString::to_string).collect();
    let mut expected: Vec<&str> = SENTENCES.to_vec();
    let mut sorted: Vec<&str> = texts.iter().map(String::as_str).collect();
    expected.sort_unstable();
    sorted.sort_unstable();
    assert_eq!(sorted, expected, "the list of sentences and the variants agree");
    for error in all {
        assert_eq!(error.to_string(), error.as_str());
    }
}

#[test]
fn debug_of_a_name_shows_its_length_only() {
    let name = ContextName::parse("prod-cluster-secret").expect("a plain name");
    let shown = format!("{name:?}");
    assert!(!shown.contains("prod-cluster-secret"), "Debug holds the name: {shown}");
    assert!(shown.contains("19"), "Debug names the length: {shown}");
}
