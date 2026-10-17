//! Names that come from a cluster, an account, a directory or a synced file are another party's text. This file is the
//! corpus that proves none of them can become an option of a tool, break a terminal or be echoed back by a refusal.
//!
//! The inputs with special characters are built from code points (`ch`), never typed, so that the file itself holds no
//! hidden character. A refusal is judged twice: by its outcome and by its text, which has to be one of a fixed list of
//! sentences and must not contain the input.

use holzwave_core::integrate::args::{
    flag_eq, flag_value, lines, ArgError, ContainerName, ContextName, EnvName, Namespace, NodeName, PodName, ProfileId,
    SectionName, SubscriptionId,
};

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
            (Ok(_), false) => {
                panic!("row {index}: a name that must be refused was accepted (input of {} bytes)", input.len())
            }
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

// ---------------------------------------------------------------------------------------------------------------------
// The other kinds. Each table says what is kept (as given, or lower-cased for a GUID) and why a neighbour is refused.
// ---------------------------------------------------------------------------------------------------------------------

type Parsed = Result<String, ArgError>;

/// Runs one kind over its table: `accepted` is `(input, stored text)`, `refused` is `(input, reason)`.
fn table(kind: &str, parse: fn(&str) -> Parsed, accepted: &[(&str, &str)], refused: &[(String, ArgError)]) {
    for (input, stored) in accepted {
        match parse(input) {
            Ok(value) => assert_eq!(&value, stored, "{kind}: the stored text of an accepted input"),
            Err(error) => panic!("{kind}: an input that must be accepted was refused ({} bytes): {error}", input.len()),
        }
    }
    for (index, (input, reason)) in refused.iter().enumerate() {
        match parse(input) {
            Ok(_) => panic!("{kind}: refusal row {index} was accepted (input of {} bytes)", input.len()),
            Err(error) => {
                assert_eq!(error, *reason, "{kind}: refusal row {index} has another reason");
                assert_fixed_sentence(error, input);
            }
        }
    }
}

/// Inputs that are stored exactly as given.
fn same<'a>(inputs: &[&'a str]) -> Vec<(&'a str, &'a str)> {
    inputs.iter().map(|input| (*input, *input)).collect()
}

fn refuse(input: impl Into<String>, reason: ArgError) -> (String, ArgError) {
    (input.into(), reason)
}

fn namespace(s: &str) -> Parsed {
    Namespace::parse(s).map(|v| v.as_str().to_owned())
}
fn container(s: &str) -> Parsed {
    ContainerName::parse(s).map(|v| v.as_str().to_owned())
}
fn pod(s: &str) -> Parsed {
    PodName::parse(s).map(|v| v.as_str().to_owned())
}
fn node(s: &str) -> Parsed {
    NodeName::parse(s).map(|v| v.as_str().to_owned())
}
fn subscription(s: &str) -> Parsed {
    SubscriptionId::parse(s).map(|v| v.as_str().to_owned())
}
fn section(s: &str) -> Parsed {
    SectionName::parse(s).map(|v| v.as_str().to_owned())
}
fn profile(s: &str) -> Parsed {
    ProfileId::parse(s).map(|v| v.as_str().to_owned())
}
fn env_name(s: &str) -> Parsed {
    EnvName::parse(s).map(|v| v.as_str().to_owned())
}

#[test]
fn namespaces_and_containers_are_dns_labels() {
    let long = "a".repeat(63);
    let accepted = same(&["default", "kube-system", "a", "a1", "1a", long.as_str()]);
    let refused = [
        refuse("", ArgError::Empty),
        refuse("Default", ArgError::Shape),
        refuse("-a", ArgError::LeadingDash),
        refuse("--all-namespaces", ArgError::LeadingDash),
        refuse("a-", ArgError::Shape),
        refuse("a_b", ArgError::Shape),
        refuse("a.b", ArgError::Shape),
        refuse("a b", ArgError::Shape),
        refuse("a;b", ArgError::Shape),
        refuse("$(id)", ArgError::Shape),
        refuse("a".repeat(64), ArgError::TooLong),
        refuse(format!("a{}b", ch(0x200b)), ArgError::Hidden),
        refuse("a\nb", ArgError::Hidden),
        refuse("a\0b", ArgError::Hidden),
    ];
    table("namespace", namespace, &accepted, &refused);
    table("container", container, &accepted, &refused);
}

#[test]
fn pods_are_dns_subdomains() {
    let long = ["a".repeat(63), "b".repeat(63), "c".repeat(63), "d".repeat(61)].join(".");
    assert_eq!(long.len(), 253, "the longest subdomain is 253 bytes");
    let accepted = same(&["api-0", "a.b.c", "web-5d9c7f6b8-x2x4q", long.as_str()]);
    let too_long_label = format!("{}.b", "a".repeat(64));
    let refused = [
        refuse("", ArgError::Empty),
        refuse(too_long_label, ArgError::TooLong),
        refuse("a..b", ArgError::Shape),
        refuse(".a", ArgError::Shape),
        refuse("a.", ArgError::Shape),
        refuse("Api", ArgError::Shape),
        refuse("a_b", ArgError::Shape),
        refuse("-x", ArgError::LeadingDash),
        refuse("--all", ArgError::LeadingDash),
        refuse("a/b", ArgError::Shape),
        refuse("a b", ArgError::Shape),
        refuse(format!("{long}x"), ArgError::TooLong),
        refuse(format!("a{}b", ch(0x202e)), ArgError::Hidden),
    ];
    table("pod", pod, &accepted, &refused);
}

#[test]
fn nodes_are_hostnames_or_ip_literals() {
    let long = ["a".repeat(63), "b".repeat(63), "c".repeat(63), "d".repeat(61)].join(".");
    let accepted = same(&["node-1", "10.0.4.12", "::1", "2001:db8::1", "talos-1.example.com", long.as_str()]);
    let refused = [
        refuse("", ArgError::Empty),
        refuse("-n", ArgError::LeadingDash),
        refuse("--nodes=x", ArgError::LeadingDash),
        refuse("1.2.3.4.5", ArgError::Shape),
        refuse("10.0.4.12/24", ArgError::Shape),
        refuse("010.0.0.1", ArgError::Shape),
        refuse("[::1]", ArgError::Shape),
        refuse("fe80::1%eth0", ArgError::Shape),
        refuse("a b", ArgError::Shape),
        refuse("Node", ArgError::Shape),
        refuse("a..b", ArgError::Shape),
        refuse("a:b", ArgError::Shape),
        refuse(format!("a{}b", ch(0x2066)), ArgError::Hidden),
        refuse("a".repeat(254), ArgError::TooLong),
    ];
    table("node", node, &accepted, &refused);
}

#[test]
fn subscriptions_are_guids_in_lower_case() {
    let guid = "0b0c7a7e-1234-4abc-8def-0123456789ab";
    let upper = "0B0C7A7E-1234-4ABC-8DEF-0123456789AB";
    let zero = "00000000-0000-0000-0000-000000000000";
    let accepted = [(guid, guid), (upper, guid), (zero, zero)];
    let refused = [
        refuse("", ArgError::Empty),
        refuse("0b0c7a7e-1234-4abc-8def", ArgError::Shape),
        refuse("0b0c7a7e12344abc8def0123456789ab", ArgError::Shape),
        refuse("0b0c7a7e-1234-4abc-8def-0123456789ag", ArgError::Shape),
        refuse(format!("{{{guid}}}"), ArgError::TooLong),
        refuse(format!("{guid}x"), ArgError::TooLong),
        refuse(format!("{guid} "), ArgError::TooLong),
        refuse(format!("-{}", &guid[1..]), ArgError::LeadingDash),
        refuse("Pay-As-You-Go", ArgError::Shape),
        refuse("0b0c7a7e_1234_4abc_8def_0123456789ab", ArgError::Shape),
        refuse("0b0c7a7e-1234-4abc-8def-0123456789a\u{1}", ArgError::Hidden),
    ];
    table("subscription", subscription, &accepted, &refused);
}

#[test]
fn sections_profiles_and_environment_names_keep_their_charsets() {
    let section_long = "a".repeat(64);
    table(
        "section",
        section,
        &same(&["default", "Prod_1", "a.b-c", "a", section_long.as_str()]),
        &[
            refuse("", ArgError::Empty),
            refuse(".hidden", ArgError::Shape),
            refuse("-x", ArgError::LeadingDash),
            refuse("a b", ArgError::Shape),
            refuse("a/b", ArgError::Shape),
            refuse("a;b", ArgError::Shape),
            refuse("a".repeat(65), ArgError::TooLong),
            refuse(format!("a{}b", ch(0x200b)), ArgError::Hidden),
        ],
    );
    let profile_long = "a".repeat(32);
    table(
        "profile",
        profile,
        &same(&["prod", "kube-prod_1", "a", "_x", profile_long.as_str()]),
        &[
            refuse("", ArgError::Empty),
            refuse("-x", ArgError::LeadingDash),
            refuse("Prod", ArgError::Shape),
            refuse("a.b", ArgError::Shape),
            refuse("a b", ArgError::Shape),
            refuse("a".repeat(33), ArgError::TooLong),
            refuse(format!("a{}b", ch(0x202e)), ArgError::Hidden),
        ],
    );
    let env_long = "A".repeat(64);
    table(
        "environment name",
        env_name,
        &same(&["KUBE_X", "_X", "A1", "A", env_long.as_str()]),
        &[
            refuse("", ArgError::Empty),
            refuse("-A", ArgError::LeadingDash),
            refuse("lower", ArgError::Shape),
            refuse("1A", ArgError::Shape),
            refuse("A-B", ArgError::Shape),
            refuse("A B", ArgError::Shape),
            refuse("A=B", ArgError::Shape),
            refuse("A".repeat(65), ArgError::TooLong),
            refuse(format!("A{}B", ch(0x200b)), ArgError::Hidden),
        ],
    );
}

#[test]
fn lines_and_flags_are_plain_scalars() {
    for (input, value) in [("1", 1u16), ("200", 200), ("5000", 5000), ("42", 42)] {
        assert_eq!(lines(input), Ok(value), "lines {input}");
    }
    let refused = [
        ("", ArgError::Empty),
        ("0", ArgError::Shape),
        ("5001", ArgError::Shape),
        ("-5", ArgError::LeadingDash),
        ("+5", ArgError::Shape),
        (" 5", ArgError::Shape),
        ("5 ", ArgError::Shape),
        ("5.0", ArgError::Shape),
        ("1e3", ArgError::Shape),
        ("007", ArgError::Shape),
        ("99999999999999999999", ArgError::TooLong),
        ("5\n", ArgError::Hidden),
        ("\u{663}", ArgError::Shape),
    ];
    for (input, reason) in refused {
        let error = lines(input).expect_err("a refused count");
        assert_eq!(error, reason, "lines refusal for an input of {} bytes", input.len());
        assert_fixed_sentence(error, input);
    }
    assert_eq!(flag_value("true"), Ok(true));
    assert_eq!(flag_value("false"), Ok(false));
    for input in ["True", "FALSE", "1", "0", "yes", " true", "true ", "true\n", ""] {
        let error = flag_value(input).expect_err("a refused flag");
        assert_fixed_sentence(error, input);
    }
}

#[test]
fn a_kind_has_no_constructor_other_than_parse_and_shows_its_length_only() {
    let seed = "very-secret-name";
    let shown = [
        format!("{:?}", Namespace::parse(seed).expect("a label")),
        format!("{:?}", ContainerName::parse(seed).expect("a label")),
        format!("{:?}", PodName::parse(seed).expect("a label")),
        format!("{:?}", NodeName::parse(seed).expect("a label")),
        format!("{:?}", SectionName::parse(seed).expect("a section")),
        format!("{:?}", ProfileId::parse(seed).expect("a profile")),
        format!("{:?}", SubscriptionId::parse("0b0c7a7e-1234-4abc-8def-0123456789ab").expect("a guid")),
        format!("{:?}", EnvName::parse("VERY_SECRET").expect("an environment name")),
    ];
    for text in shown {
        let holds_text = ["secret", "SECRET", "0b0c7a7e"].iter().any(|part| text.contains(part));
        assert!(!holds_text, "Debug holds text: {text}");
        assert!(text.contains("len="), "Debug names a length: {text}");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The sweep: generated, not listed.
// ---------------------------------------------------------------------------------------------------------------------

/// Every kind as a function of the text only.
fn every_kind() -> Vec<(&'static str, fn(&str) -> Parsed)> {
    fn context(s: &str) -> Parsed {
        ContextName::parse(s).map(|v| v.as_str().to_owned())
    }
    fn count(s: &str) -> Parsed {
        lines(s).map(|v| v.to_string())
    }
    fn flag(s: &str) -> Parsed {
        flag_value(s).map(|v| v.to_string())
    }
    vec![
        ("context", context),
        ("namespace", namespace),
        ("container", container),
        ("pod", pod),
        ("node", node),
        ("subscription", subscription),
        ("section", section),
        ("profile", profile),
        ("environment name", env_name),
        ("lines", count),
        ("flag", flag),
    ]
}

/// Valid names of every kind plus the hostile corpus.
fn seeds() -> Vec<String> {
    let mut seeds: Vec<String> = [
        "dev",
        "kube-system",
        "api-0",
        "a.b.c",
        "10.0.4.12",
        "::1",
        "2001:db8::1",
        "0b0c7a7e-1234-4abc-8def-0123456789ab",
        "default",
        "my_section",
        "prod",
        "KUBE_X",
        "200",
        "true",
        "a",
        "a1",
        "node-1",
        "talos-1.example.com",
        "arn:aws:eks:eu-central-1:000000000000:cluster/prod",
        "gke_proj_europe-west1_cluster-1",
        "user@cluster",
        "a b",
        "a=b",
        "",
        "-x",
        "--server=https://x",
        "--all-namespaces",
        "a..b",
        ".a",
        "a.",
        "Default",
        "a_b",
        "1.2.3.4.5",
        "10.0.4.12/24",
        "[::1]",
        "$(id)",
        "`id`",
        "a;b",
        "a|b",
        "../x",
        "a/b",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    seeds.push("a".repeat(253));
    seeds.push("a".repeat(254));
    seeds
}

/// One of each class of character a name may never hold.
const HIDDEN: &[u32] = &[
    0x00, 0x01, 0x09, 0x0a, 0x0d, 0x1b, 0x7f, 0x85, 0xad, 0xa0, 0x61c, 0x200b, 0x200c, 0x200d, 0x200e, 0x200f, 0x2003,
    0x2028, 0x2029, 0x202a, 0x202e, 0x2060, 0x2066, 0x2069, 0x3000, 0xfeff, 0xe0001, 0xe0041, 0xe007f,
];

#[test]
fn no_kind_accepts_an_input_that_starts_with_a_dash_or_holds_a_hidden_character() {
    let seeds = seeds();
    assert!(seeds.len() >= 40, "the sweep starts from at least 40 seeds");
    let mut checked = 0usize;
    for (kind, parse) in every_kind() {
        for seed in &seeds {
            // What a kind accepts as it is never starts with a dash and never holds a hidden character.
            if let Ok(kept) = parse(seed) {
                assert!(!kept.starts_with('-'), "{kind}: an accepted value starts with a dash");
                let holds_hidden = kept.chars().any(|c| HIDDEN.contains(&u32::from(c)));
                assert!(!holds_hidden, "{kind}: an accepted value holds a hidden character");
            }
            let dashed = format!("-{seed}");
            assert!(parse(&dashed).is_err(), "{kind}: a dash in front of a seed of {} bytes was accepted", seed.len());
            checked += 1;
            let chars: Vec<char> = seed.chars().collect();
            for &code in HIDDEN {
                let hidden = ch(code);
                let middle = chars.len() / 2;
                let at_start: String = std::iter::once(hidden).chain(chars.iter().copied()).collect();
                let (head, tail) = chars.split_at(middle);
                let in_middle: String =
                    head.iter().copied().chain(std::iter::once(hidden)).chain(tail.iter().copied()).collect();
                let at_end: String = chars.iter().copied().chain(std::iter::once(hidden)).collect();
                for (place, input) in [("start", at_start), ("middle", in_middle), ("end", at_end)] {
                    match parse(&input) {
                        Ok(_) => {
                            panic!("{kind}: U+{code:04X} at the {place} of a seed of {} bytes was accepted", seed.len())
                        }
                        Err(error) => assert_fixed_sentence(error, &input),
                    }
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 10_000, "the sweep is generated: {checked} inputs");
}
