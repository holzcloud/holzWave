//! The profiles of the integrations: what holzWave remembers about a connection to a cluster, a subscription or a
//! domain, and the machine-local file with the paths of one computer.
//!
//! `integrations.json` is a synced file, so every other device reads it: it is validated like hostile input, by the
//! strictest platform's rule, and a file that fails any rule is quarantined whole. Its contract is published as
//! `docs/integrations.schema.json` and held equal to the loader by a parity corpus in both directions.

mod common;
#[path = "common/schema_check.rs"]
mod schema_check;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::Xorshift64;
use holzwave_core::integrate::args::{ContextName, EnvName, ProfileId};
use holzwave_core::integrate::profiles::{
    secret_account, IntegrationProfile, IntegrationProfiles, APP_SET_NAMES, FILE, MAX_BYTES, MAX_ENV, MAX_NAME,
    MAX_PROFILES, MAX_SECRET, MAX_SECRET_NAMES, MAX_VALUE, PROGRAM_NAMES,
};
use holzwave_core::integrate::run::{EnvExtra, EnvPolicy};
use holzwave_core::integrate::ToolId;
use holzwave_core::platform::Os;
use holzwave_core::secrets::{MemoryStore, SecretStore};
use holzwave_core::Error;
use serde_json::{json, Value};
use zeroize::Zeroizing;

const SCHEMA_PATH: &str = "docs/integrations.schema.json";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent: the workspace root").to_path_buf()
}

/// The schema file, parsed once and linted before it is returned.
fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| schema_check::load_schema(&root(), SCHEMA_PATH))
}

/// What the schema says about the text of a document.
fn schema_verdict(text: &str) -> Result<(), String> {
    schema_check::schema_verdict(schema(), SCHEMA_PATH, text)
}

/// What the loader says about the text of a document.
fn loader_verdict(text: &str) -> Result<IntegrationProfiles, String> {
    IntegrationProfiles::parse(text)
}

/// Both sides must give the same verdict; says whether they accepted.
fn same_verdict(label: &str, text: &str) -> bool {
    let (loaded, schema) = (loader_verdict(text), schema_verdict(text));
    assert!(
        loaded.is_ok() == schema.is_ok(),
        "{label}: the loader and the schema differ\n  document: {text}\n  loader: {:?}\n  schema: {schema:?}",
        loaded.as_ref().map(|_| "accepted")
    );
    loaded.is_ok()
}

/// A scratch directory that is removed when the test ends, whatever its outcome.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        Self(common::scratch_dir(tag))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(text: &str) -> ProfileId {
    ProfileId::parse(text).expect("a valid profile id")
}

const ARN: &str = "arn:aws:eks:eu-central-1:123456789012:cluster/prod";

/// The tracer profile: a Kubernetes profile that pins an ARN context and a namespace and says production.
fn production_kube() -> IntegrationProfile {
    let mut profile = IntegrationProfile::new(id("prod"), ToolId::Kube, "Production");
    profile.context = Some(ContextName::parse(ARN).expect("a context"));
    profile.namespace = Some(holzwave_core::integrate::args::Namespace::parse("payments").expect("a namespace"));
    profile.production = true;
    profile
}

fn tracer_document() -> Value {
    json!({
        "version": 1,
        "profiles": [{
            "id": "prod", "tool": "kube", "name": "Production", "context": ARN, "namespace": "payments",
            "production": true,
        }]
    })
}

#[test]
fn a_profile_survives_save_and_load() {
    let dir = Scratch::new("profiles-roundtrip");
    let (empty, notes) = IntegrationProfiles::load(dir.path());
    assert!(empty.items.is_empty() && notes.is_empty(), "a missing file is an empty set with no note");
    assert!(!dir.path().join(FILE).exists(), "loading writes nothing");

    let mut set = IntegrationProfiles::default();
    set.upsert(production_kube()).expect("the profile is valid");
    set.save(dir.path()).expect("save");

    let text = fs::read_to_string(dir.path().join(FILE)).expect("the file");
    assert!(text.ends_with('\n'), "the file ends in a newline");
    let document: Value = serde_json::from_str(&text).expect("the file is JSON");
    assert_eq!(document["version"], json!(1));
    assert_eq!(document["profiles"][0]["id"], json!("prod"));
    assert!(!dir.path().join(format!("{FILE}.tmp")).exists(), "the temporary file is gone: the write was atomic");

    let (back, notes) = IntegrationProfiles::load(dir.path());
    assert!(notes.is_empty(), "no note for a clean file: {notes:?}");
    assert_eq!(back, set);
    assert_eq!(back.get("prod"), Some(&production_kube()));

    // Written again, the same bytes: a save of what was loaded changes nothing, so a sync sees no edit.
    back.save(dir.path()).expect("save again");
    assert_eq!(fs::read_to_string(dir.path().join(FILE)).expect("the file"), text);
}

#[test]
fn the_first_schema_and_the_loader_agree_on_one_profile() {
    let document = tracer_document().to_string();
    assert!(same_verdict("the tracer document", &document), "the loader and the schema accept the tracer document");
    let loaded = loader_verdict(&document).expect("the loader accepts it");
    assert_eq!(loaded.items, vec![production_kube()]);

    let wrong = {
        let mut doc = tracer_document();
        doc["profiles"][0]["production"] = json!("yes");
        doc.to_string()
    };
    assert!(!same_verdict("production as text", &wrong), "both refuse a production flag that is text");

    // What the loader writes is a document the schema accepts.
    let mut set = IntegrationProfiles::default();
    set.upsert(production_kube()).expect("valid");
    assert_eq!(schema_verdict(&set.to_json()), Ok(()), "the written file is accepted by the schema");
}

#[test]
fn a_pinned_context_is_production_when_a_profile_says_so() {
    let mut set = IntegrationProfiles::default();
    set.upsert(production_kube()).expect("valid");
    let pinned = ContextName::parse(ARN).expect("a context");
    let other = ContextName::parse("kind-dev").expect("a context");
    assert!(set.production_for(ToolId::Kube, &pinned));
    assert!(!set.production_for(ToolId::Kube, &other), "another context is not production");
    assert!(!set.production_for(ToolId::Talos, &pinned), "the same name for another tool is not production");

    // A profile that pins the context and does not say production does not undo the other one.
    let mut calm = IntegrationProfile::new(id("prod-calm"), ToolId::Kube, "Calm");
    calm.context = Some(pinned.clone());
    set.upsert(calm).expect("valid");
    assert!(set.production_for(ToolId::Kube, &pinned), "any profile that pins the context and says so decides");
    set.remove("prod");
    assert!(!set.production_for(ToolId::Kube, &pinned), "without that profile the context is not production");
}

// ---------------------------------------------------------------------------------------------------------------------
// Documents and notes.
// ---------------------------------------------------------------------------------------------------------------------

fn ch(code_point: u32) -> char {
    char::from_u32(code_point).expect("a scalar value")
}

fn rep(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

/// A file with these profiles.
fn file_of(profiles: Vec<Value>) -> String {
    json!({ "version": 1, "profiles": profiles }).to_string()
}

/// A valid Kubernetes profile document with `members` set on top.
fn kube_doc(members: &[(&str, Value)]) -> Value {
    let mut doc = json!({ "id": "p", "tool": "kube", "name": "A profile" });
    for (key, value) in members {
        doc[*key] = value.clone();
    }
    doc
}

fn one(members: &[(&str, Value)]) -> String {
    file_of(vec![kube_doc(members)])
}

/// The note of a refused document.
fn refused(text: &str) -> String {
    match loader_verdict(text) {
        Err(note) => note,
        Ok(_) => panic!("the loader accepted a document that must be refused: {text}"),
    }
}

fn accepted(text: &str) -> IntegrationProfiles {
    loader_verdict(text).unwrap_or_else(|note| panic!("the loader refused a document that is valid ({note}): {text}"))
}

#[test]
fn ids_names_and_tools_are_validated() {
    let longest = rep('a', 32);
    for good in ["a", "prod-1", "x_y", "0", "_", longest.as_str()] {
        accepted(&one(&[("id", json!(good))]));
    }
    let hostile_ids = [
        String::new(),
        "A".into(),
        "-a".into(),
        "a b".into(),
        "a.b".into(),
        "a/b".into(),
        rep('a', 33),
        format!("a{}", ch(0xe9)),
        format!("a{}", ch(0)),
        "a\n".into(),
    ];
    for bad in &hostile_ids {
        let note = refused(&one(&[("id", json!(bad))]));
        assert_eq!(note, "profile 1: the id is not valid", "the position stands in for an id that is not valid");
    }
    let note = refused(&file_of(vec![kube_doc(&[]), kube_doc(&[("name", json!("Other"))])]));
    assert_eq!(note, "profile p: the id is used twice");

    for tool in ["helm", "Kube", "KUBE", "kubectl", "", "kube ", "gam7"] {
        refused(&one(&[("tool", json!(tool))]));
    }
    for tool in ["kube", "talos", "azure", "gam"] {
        accepted(&one(&[("tool", json!(tool))]));
    }
    refused(&one(&[("tool", json!(1))]));
    refused(&one(&[("tool", Value::Null)]));

    // The display name: 1 to 64 characters, no control and no hidden character.
    let (longest, wide) = (rep('n', MAX_NAME), rep(ch(0x4e2d), MAX_NAME));
    for good in ["N", longest.as_str(), "Production (eu) ", "Prod \u{2601}", wide.as_str()] {
        accepted(&one(&[("name", json!(good))]));
    }
    let hostile_names = [
        String::new(),
        rep('n', MAX_NAME + 1),
        format!("a{}b", ch(0x0a)),
        format!("a{}b", ch(0x09)),
        format!("a{}b", ch(0x00)),
        format!("a{}b", ch(0x7f)),
        format!("a{}b", ch(0x85)),
        format!("a{}b", ch(0x202e)),
        format!("a{}b", ch(0x200b)),
        format!("a{}b", ch(0xfeff)),
        format!("a{}b", ch(0xe0041)),
        format!("a{}b", ch(0x2066)),
    ];
    for bad in &hostile_names {
        assert_eq!(refused(&one(&[("name", json!(bad))])), "profile p: name is not valid");
    }
    refused(&one(&[("name", json!(5))]));
    // A document without its required members.
    for missing in ["id", "tool", "name"] {
        let mut doc = kube_doc(&[]);
        doc.as_object_mut().expect("object").remove(missing);
        refused(&file_of(vec![doc]));
    }
}

/// The selection fields that each tool takes, written out here and not read from the code.
const SELECTS: [(&str, &[&str]); 4] = [
    ("kube", &["context", "namespace"]),
    ("talos", &["context", "node"]),
    ("azure", &["subscription"]),
    ("gam", &["section"]),
];

const FIELD_VALUES: [(&str, &str); 5] = [
    ("context", "arn:aws:eks:eu-central-1:123456789012:cluster/x"),
    ("namespace", "payments"),
    ("subscription", "0b4a1d7c-9a3e-4a5e-8f3d-2c6b1e9f7a10"),
    ("section", "work"),
    ("node", "10.0.0.7"),
];

#[test]
fn selection_fields_belong_to_the_tool() {
    for (tool, takes) in SELECTS {
        for (field, value) in FIELD_VALUES {
            let text = file_of(vec![kube_doc(&[("tool", json!(tool)), (field, json!(value))])]);
            if takes.contains(&field) {
                accepted(&text);
            } else {
                assert_eq!(refused(&text), format!("profile p: {field} is not used by {tool} profiles"));
            }
            // An explicit null is the same as absent for every field of every tool.
            accepted(&file_of(vec![kube_doc(&[("tool", json!(tool)), (field, Value::Null)])]));
        }
    }
    // `isolated` is for Azure and GAM only; a Kubernetes or Talos profile cannot carry it.
    for tool in ["azure", "gam"] {
        accepted(&file_of(vec![kube_doc(&[("tool", json!(tool)), ("isolated", json!(true))])]));
    }
    for tool in ["kube", "talos"] {
        let note = refused(&file_of(vec![kube_doc(&[("tool", json!(tool)), ("isolated", json!(true))])]));
        assert_eq!(note, "profile p: isolated is only for azure and gam profiles");
        accepted(&file_of(vec![kube_doc(&[("tool", json!(tool)), ("isolated", json!(false))])]));
    }
    // The value of a field that belongs is validated by its kind.
    let by_kind: [(&str, &str, Value); 12] = [
        ("kube", "context", json!("-x")),
        ("kube", "context", json!("")),
        ("kube", "context", json!(" x")),
        ("kube", "context", json!(format!("a{}b", ch(0x202e)))),
        ("kube", "context", json!(rep('c', 254))),
        ("kube", "namespace", json!("Bad_NS")),
        ("kube", "namespace", json!("-ns")),
        ("talos", "node", json!("1.2.3.4.5")),
        ("talos", "node", json!("a..b")),
        ("azure", "subscription", json!("my subscription")),
        ("gam", "section", json!(".hidden")),
        ("gam", "section", json!("a/b")),
    ];
    for (tool, field, value) in by_kind {
        let note = refused(&file_of(vec![kube_doc(&[("tool", json!(tool)), (field, value)])]));
        assert_eq!(note, format!("profile p: {field} is not valid"));
    }
    // A field of the wrong kind is refused as a whole, and a number is not a context.
    refused(&one(&[("context", json!(7))]));
    refused(&one(&[("namespace", json!(["a"]))]));
    refused(&one(&[("production", json!("yes"))]));
    refused(&one(&[("production", Value::Null)]));
    refused(&one(&[("isolated", json!(0))]));
}

/// The names of the plan's rule, every one of them refused in a synced profile.
const DENIED: &[&str] = &[
    "HOLZWAVE_X",
    "LD_PRELOAD",
    "DYLD_INSERT_LIBRARIES",
    "PYTHONSTARTUP",
    "NODE_OPTIONS",
    "GIT_SSH_COMMAND",
    "BASH_ENV",
    "ENV",
    "IFS",
    "PATH",
    "KUBECONFIG",
    "TALOSCONFIG",
    "AZURE_CONFIG_DIR",
    "GAMCFGDIR",
    "GAMCFGSECTION",
    "BROWSER",
    "EDITOR",
    "VISUAL",
    "PAGER",
    "SHELL",
    "SHELLOPTS",
    "BASHOPTS",
    "PROMPT_COMMAND",
    "ZDOTDIR",
    "HOME",
    "XDG_CONFIG_HOME",
    "KUBE_EDITOR",
    "KUBECTL_EXTERNAL_DIFF",
    "AZURE_EXTENSION_DIR",
    "SSLKEYLOGFILE",
];

#[test]
fn environment_names_are_validated_for_every_platform() {
    for name in DENIED {
        let note = refused(&one(&[("env", json!({ *name: "x" }))]));
        assert_eq!(note, format!("profile p: env {name} is not allowed"), "{name}");
    }
    // Any name below a denied prefix, not only the ones listed.
    for name in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "PYTHONPATH", "NODE_PATH", "GIT_DIR", "HOLZWAVE_TOKEN"] {
        assert_eq!(refused(&one(&[("env", json!({ name: "x" }))])), format!("profile p: env {name} is not allowed"));
    }
    // The Windows case rule: names that Windows reads without regard to case are refused on every platform, and they
    // are not echoed because they are not valid names either.
    for name in ["Path", "path", "Ld_Preload", "ld_preload", "Holzwave_X", "bash_env", "Env", "pythonstartup", "Home"] {
        let note = refused(&one(&[("env", json!({ name: "x" }))]));
        assert_eq!(note, "profile p: an env name is not allowed", "{name}");
        assert_eq!(refused(&one(&[("secret_env", json!([name]))])), "profile p: an env name is not allowed");
    }
    // Not names at all.
    for name in [
        String::new(),
        "aws_profile".into(),
        "1A".into(),
        "A-B".into(),
        "A B".into(),
        "A=B".into(),
        format!("A{}", ch(0xc4)),
        rep('A', 65),
        "A\n".into(),
        format!("A{}", ch(0)),
    ] {
        let note = refused(&one(&[("env", json!({ name.clone(): "x" }))]));
        assert_eq!(note, "profile p: an env name is not allowed", "{name:?}");
    }
    for (name, value) in [
        ("AWS_PROFILE", "prod"),
        ("AZURE_CORE_ONLY_SHOW_ERRORS", "true"),
        ("HTTPS_PROXY", "https://proxy.example.com:8080"),
        ("_X", "1"),
        ("A1", ""),
        ("NO_COLOR", "1"),
    ] {
        let set = accepted(&one(&[("env", json!({ name: value }))]));
        assert_eq!(set.items[0].env.get(name).map(String::as_str), Some(value));
    }
    // 32 variables are accepted and 33 are not.
    let many = |n: usize| (0..n).map(|i| (format!("VAR_{i:02}"), json!("v"))).collect::<serde_json::Map<_, _>>();
    accepted(&one(&[("env", Value::Object(many(MAX_ENV)))]));
    assert_eq!(refused(&one(&[("env", Value::Object(many(MAX_ENV + 1)))])), "profile p: env has more than 32 variables");
    refused(&one(&[("env", json!(["A=b"]))]));
    refused(&one(&[("env", Value::Null)]));
    refused(&one(&[("env", json!({ "A": 1 }))]));
}

/// The deny rule and the list of names are the runner's, asked for both families, and the list that is only for
/// synced values does not change what the person's own environment passes.
#[test]
fn the_runners_rule_and_the_profile_rule_agree() {
    for os in [Os::Macos, Os::Linux, Os::Windows] {
        for name in PROGRAM_NAMES {
            assert!(
                !EnvPolicy::is_denied(name, os),
                "{name} reaches a child from the person's own environment; only a synced profile may not set it"
            );
        }
        for name in APP_SET_NAMES {
            assert!(!EnvPolicy::is_denied(name, os), "{name} is an app name, not a loader name");
        }
    }
    // The names the app sets itself are the runner's: the runner refuses each of them in a profile, and this module
    // lists exactly those.
    let runner_refuses: Vec<&str> = ["KUBECONFIG", "TALOSCONFIG", "AZURE_CONFIG_DIR", "GAMCFGDIR", "GAMCFGSECTION"]
        .into_iter()
        .filter(|name| {
            let extra = EnvExtra { plain: vec![((*name).to_owned(), "x".to_owned())], ..EnvExtra::default() };
            EnvPolicy::build(std::iter::empty(), Os::Linux, "/usr/bin".into(), extra).is_err()
        })
        .collect();
    let mut listed = APP_SET_NAMES.to_vec();
    listed.sort_unstable();
    let mut expected = runner_refuses.clone();
    expected.sort_unstable();
    assert_eq!(listed, expected, "the app-set names of the profile rule and of the runner differ");
    // A name the runner denies is refused here for each family, and a case that only Windows folds is still refused.
    assert!(EnvPolicy::is_denied("Path", Os::Windows) && !EnvPolicy::is_denied("Path", Os::Macos));
    assert!(EnvPolicy::is_denied("Ld_Preload", Os::Windows) && !EnvPolicy::is_denied("Ld_Preload", Os::Linux));
    refused(&one(&[("env", json!({ "Path": "x" }))]));
    refused(&one(&[("env", json!({ "Ld_Preload": "x" }))]));
    // The lists are what the tests above pin.
    let mut listed_all: Vec<&str> = PROGRAM_NAMES.iter().chain(APP_SET_NAMES.iter()).copied().collect();
    listed_all.sort_unstable();
    for name in listed_all {
        assert!(DENIED.contains(&name), "{name} is refused but not pinned by the test list");
    }
}

#[test]
fn environment_values_are_bounded_and_not_paths() {
    let value = |v: &str| one(&[("env", json!({ "A": v }))]);
    accepted(&value(&rep('v', MAX_VALUE)));
    assert_eq!(refused(&value(&rep('v', MAX_VALUE + 1))), "profile p: env A has a value that is too long");
    // The bound counts bytes: 512 two-byte characters fit, 513 do not.
    accepted(&value(&rep(ch(0xe9), MAX_VALUE / 2)));
    refused(&value(&rep(ch(0xe9), MAX_VALUE / 2 + 1)));

    for code in [0x00, 0x01, 0x07, 0x09, 0x0a, 0x0d, 0x1b, 0x7f, 0x85, 0x9f] {
        let note = refused(&value(&format!("a{}b", ch(code))));
        assert_eq!(note, "profile p: env A has a value with a control or invisible character", "U+{code:04X}");
    }
    for code in [0x202e, 0x200b, 0xfeff, 0x2066, 0x00ad, 0xe0041] {
        refused(&value(&format!("a{}b", ch(code))));
    }
    let looks_like_a_path = [
        "/etc/passwd",
        "/",
        "~/bin",
        "\\\\server\\share",
        "\\\\?\\C:\\x",
        "C:\\Users",
        "c:/Users",
        "Z:\\",
        "d:/",
    ];
    for path in looks_like_a_path {
        let note = refused(&value(path));
        assert_eq!(note, "profile p: env A has a value that looks like a path", "{path}");
    }
    for fine in [
        "https://proxy.example.com:8080",
        "~",
        "~x",
        "a/b",
        "C",
        "C:",
        "C:x",
        "ab:\\x",
        "\\x",
        "x\\\\y",
        "",
        "1",
        "true",
        "eu-central-1",
        " /leading space",
    ] {
        accepted(&value(fine));
    }
}

#[test]
fn secret_names_follow_the_same_rules() {
    for name in DENIED {
        let note = refused(&one(&[("secret_env", json!([name]))]));
        assert_eq!(note, format!("profile p: secret_env {name} is not allowed"), "{name}");
    }
    for name in ["AWS_SECRET_ACCESS_KEY", "AZURE_CLIENT_SECRET", "_S", "S1"] {
        let set = accepted(&one(&[("secret_env", json!([name]))]));
        assert_eq!(set.items[0].secret_env, vec![name.to_owned()]);
    }
    let too_long = rep('A', 65);
    for name in ["secret", "1S", "A-B", "", "A B", too_long.as_str()] {
        assert_eq!(refused(&one(&[("secret_env", json!([name]))])), "profile p: an env name is not allowed");
    }
    let many = |n: usize| (0..n).map(|i| json!(format!("SECRET_{i}"))).collect::<Vec<_>>();
    accepted(&one(&[("secret_env", json!(many(MAX_SECRET_NAMES)))]));
    assert_eq!(
        refused(&one(&[("secret_env", json!(many(MAX_SECRET_NAMES + 1)))])),
        "profile p: secret_env has more than 8 names"
    );
    assert_eq!(
        refused(&one(&[("secret_env", json!(["TOKEN", "TOKEN"]))])),
        "profile p: secret_env TOKEN is listed twice"
    );
    assert_eq!(
        refused(&one(&[("env", json!({ "TOKEN": "x" })), ("secret_env", json!(["TOKEN"]))])),
        "profile p: secret_env TOKEN is also a plain variable"
    );
    refused(&one(&[("secret_env", json!("TOKEN"))]));
    refused(&one(&[("secret_env", json!([1]))]));
    refused(&one(&[("secret_env", Value::Null)]));
    // A secret name next to plain variables of other names is fine.
    accepted(&one(&[("env", json!({ "AWS_PROFILE": "prod" })), ("secret_env", json!(["AWS_SECRET_ACCESS_KEY"]))]));
}

/// A secret value in every position of a document: no note may hold any of them.
const CANARY: &str = "HW-CANARY";

fn assert_no_canary(label: &str, note: &str) {
    assert!(!note.contains("CANARY"), "{label}: the note holds a value of the document: {note}");
    assert!(!note.contains(CANARY), "{label}: the note holds a value of the document: {note}");
}

#[test]
fn a_damaged_file_is_quarantined_whole_with_a_note_without_values() {
    let long_name = rep('n', MAX_NAME + 1);
    let damaged: Vec<(&str, Vec<u8>)> = vec![
        ("not JSON", b"HW-CANARY not json".to_vec()),
        ("an array", b"[\"HW-CANARY\"]".to_vec()),
        ("a string", b"\"HW-CANARY\"".to_vec()),
        ("another version", br#"{"version":2,"profiles":[],"HW-CANARY-NEW":1}"#.to_vec()),
        ("a missing version", br#"{"profiles":[]}"#.to_vec()),
        ("a version as text", br#"{"version":"1","profiles":[]}"#.to_vec()),
        ("a unknown member", br#"{"version":1,"profiles":[],"HW-CANARY-MEMBER":1}"#.to_vec()),
        ("a wrong type", one(&[("production", json!("HW-CANARY-YES"))]).into_bytes()),
        ("a denied name", one(&[("env", json!({ "LD_PRELOAD": "HW-CANARY-ENV-0001" }))]).into_bytes()),
        ("a path value", one(&[("env", json!({ "A": "/HW-CANARY/path" }))]).into_bytes()),
        ("a long name", one(&[("name", json!(long_name))]).into_bytes()),
        ("not UTF-8", vec![0xff, 0xfe, b'{']),
        ("a good profile and a bad one", file_of(vec![kube_doc(&[]), kube_doc(&[("id", json!("q")), ("tool", json!("HW-CANARY-TOOL"))])]).into_bytes()),
    ];
    for (label, bytes) in damaged {
        let dir = Scratch::new("profiles-quarantine");
        let path = dir.path().join(FILE);
        fs::write(&path, &bytes).expect("write");
        let (set, notes) = IntegrationProfiles::load(dir.path());
        assert!(set.items.is_empty(), "{label}: the app starts without profiles");
        assert_eq!(notes.len(), 1, "{label}: one plain note: {notes:?}");
        assert_no_canary(label, &notes[0]);
        assert!(!notes[0].is_empty() && notes[0].len() < 200, "{label}: the note is one short sentence: {notes:?}");
        let bad = dir.path().join(format!("{FILE}.bad"));
        assert_eq!(fs::read(&bad).expect("the .bad copy"), bytes, "{label}: the damaged file is kept as it was");
        assert_eq!(fs::read(&path).expect("the file"), bytes, "{label}: the file itself is not touched by a load");

        // The next save of an empty set does not destroy the copy of the damaged file.
        set.save(dir.path()).expect("save the empty set");
        assert_eq!(fs::read(&bad).expect("the .bad copy"), bytes, "{label}: the save left the copy alone");
        let (again, notes) = IntegrationProfiles::load(dir.path());
        assert!(again.items.is_empty() && notes.is_empty(), "{label}: the saved empty set loads clean: {notes:?}");
        assert_eq!(fs::read(&bad).expect("the .bad copy"), bytes, "{label}: a clean load leaves the copy alone");
    }

    // The notes name the profile and the field.
    assert_eq!(refused(&one(&[("env", json!({ "LD_PRELOAD": "x" }))])), "profile p: env LD_PRELOAD is not allowed");

    // Absent and empty are not damage: no note and no copy.
    let dir = Scratch::new("profiles-absent");
    let (set, notes) = IntegrationProfiles::load(dir.path());
    assert!(set.items.is_empty() && notes.is_empty());
    fs::write(dir.path().join(FILE), b"").expect("write");
    let (set, notes) = IntegrationProfiles::load(dir.path());
    assert!(set.items.is_empty() && notes.is_empty(), "an empty file is not damage: {notes:?}");
    assert!(!dir.path().join(format!("{FILE}.bad")).exists(), "nothing is copied for an absent or an empty file");

    // A directory in the place of the file is not a file: one note, nothing to copy.
    let dir = Scratch::new("profiles-directory");
    fs::create_dir(dir.path().join(FILE)).expect("a directory");
    let (set, notes) = IntegrationProfiles::load(dir.path());
    assert!(set.items.is_empty());
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(!dir.path().join(format!("{FILE}.bad")).exists());

    // A good file is accepted whole and a save of it is the same bytes.
    let dir = Scratch::new("profiles-good");
    let good = one(&[("env", json!({ "AWS_PROFILE": "prod" })), ("secret_env", json!(["AWS_SECRET_ACCESS_KEY"]))]);
    fs::write(dir.path().join(FILE), &good).expect("write");
    let (set, notes) = IntegrationProfiles::load(dir.path());
    assert!(notes.is_empty() && set.items.len() == 1, "{notes:?}");
    assert!(!dir.path().join(format!("{FILE}.bad")).exists());
}

#[test]
fn limits_hold() {
    let many = |n: usize| file_of((0..n).map(|i| kube_doc(&[("id", json!(format!("p{i}")))])).collect());
    assert_eq!(accepted(&many(MAX_PROFILES)).items.len(), MAX_PROFILES);
    assert_eq!(refused(&many(MAX_PROFILES + 1)), "the file holds more than 64 profiles");

    // A file of exactly the limit is read; one byte more is not, whatever it holds.
    let valid = one(&[]);
    let padded = |total: u64| format!("{valid}{}", rep(' ', usize::try_from(total).expect("size") - valid.len()));
    assert_eq!(padded(MAX_BYTES).len() as u64, MAX_BYTES);
    accepted(&padded(MAX_BYTES));
    assert_eq!(refused(&padded(MAX_BYTES + 1)), format!("{FILE} is larger than {MAX_BYTES} bytes"));

    let dir = Scratch::new("profiles-large");
    fs::write(dir.path().join(FILE), padded(MAX_BYTES + 1)).expect("write");
    let (set, notes) = IntegrationProfiles::load(dir.path());
    assert!(set.items.is_empty());
    assert_eq!(notes, vec![format!("{FILE} is larger than {MAX_BYTES} bytes")]);
    assert_eq!(
        fs::metadata(dir.path().join(format!("{FILE}.bad"))).expect("a copy").len(),
        MAX_BYTES + 1,
        "a file just over the limit is copied"
    );

    // A file far over the limit is renamed, not copied, and not read.
    let dir = Scratch::new("profiles-huge");
    let huge = fs::File::create(dir.path().join(FILE)).expect("create");
    huge.set_len(5 * 1024 * 1024).expect("a sparse file");
    drop(huge);
    let (set, notes) = IntegrationProfiles::load(dir.path());
    assert!(set.items.is_empty());
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(!dir.path().join(FILE).exists(), "the huge file is moved aside");
    assert_eq!(fs::metadata(dir.path().join(format!("{FILE}.bad"))).expect("moved").len(), 5 * 1024 * 1024);

    // A set that is within every field's bound and would still not fit in the file is refused where it is added, so
    // that the person never loses a profile file to the size limit on the next start.
    let mut set = IntegrationProfiles::default();
    let full = |i: usize| {
        let mut profile = IntegrationProfile::new(id(&format!("p{i}")), ToolId::Kube, "Big");
        for n in 0..MAX_ENV {
            profile.env.insert(format!("VAR_{n:02}"), rep('v', MAX_VALUE));
        }
        profile
    };
    let mut added = 0;
    let refusal = loop {
        match set.upsert(full(added)) {
            Ok(()) => added += 1,
            Err(note) => break note,
        }
        assert!(added < MAX_PROFILES, "the size limit was never reached");
    };
    assert!(added >= 5, "a handful of full profiles fit: {added}");
    assert_eq!(refusal, format!("the profiles would be larger than {MAX_BYTES} bytes"));
    assert_eq!(set.items.len(), added, "a refused profile leaves the set as it was");
    assert!(set.to_json().len() as u64 <= MAX_BYTES);
    let dir = Scratch::new("profiles-size");
    set.save(dir.path()).expect("what fits is written");
    let (back, notes) = IntegrationProfiles::load(dir.path());
    assert!(notes.is_empty() && back == set, "{notes:?}");
}

#[test]
fn a_set_that_is_not_valid_is_not_added_or_written() {
    let mut set = IntegrationProfiles::default();
    let mut bad = IntegrationProfile::new(id("p"), ToolId::Kube, "Bad");
    bad.env.insert("LD_PRELOAD".into(), "x".into());
    assert_eq!(set.upsert(bad.clone()), Err("profile p: env LD_PRELOAD is not allowed".to_owned()));
    assert!(set.items.is_empty());
    // The public fields can be set to anything; the set is not trusted to have been checked.
    set.items.push(bad);
    let dir = Scratch::new("profiles-invalid-save");
    assert!(set.save(dir.path()).is_err(), "a set that would not load again is not written");
    assert!(!dir.path().join(FILE).exists());
    // Replacing a profile keeps its position, and a refused replacement keeps the old one.
    let mut set = IntegrationProfiles::default();
    set.upsert(IntegrationProfile::new(id("a"), ToolId::Kube, "A")).expect("valid");
    set.upsert(IntegrationProfile::new(id("b"), ToolId::Gam, "B")).expect("valid");
    set.upsert(IntegrationProfile::new(id("a"), ToolId::Azure, "A2")).expect("valid");
    assert_eq!(set.items.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["A2", "B"]);
    let mut worse = IntegrationProfile::new(id("a"), ToolId::Kube, "A3");
    worse.isolated = true;
    assert!(set.upsert(worse).is_err());
    assert_eq!(set.get("a").map(|p| p.name.as_str()), Some("A2"));
    let mut wrong_field = IntegrationProfile::new(id("c"), ToolId::Azure, "C");
    wrong_field.context = Some(ContextName::parse("ctx").expect("a context"));
    assert_eq!(set.upsert(wrong_field), Err("profile c: context is not used by azure profiles".to_owned()));
    // Debug prints the id, the tool and counts, never an environment.
    let mut with_env = IntegrationProfile::new(id("d"), ToolId::Kube, "D");
    with_env.env.insert("AWS_PROFILE".into(), "HW-CANARY-ENV-0001".into());
    with_env.secret_env.push("TOKEN".into());
    set.upsert(with_env.clone()).expect("valid");
    assert_no_canary("debug of a profile", &format!("{with_env:?}"));
    assert_no_canary("debug of the set", &format!("{set:?}"));
    assert_no_canary("pretty debug", &format!("{set:#?} {with_env:#?}"));
}

/// Documents that each break one rule, with a canary value in the position the rule is about.
fn hostile_documents() -> Vec<(String, String)> {
    let canary = |position: &str| format!("HW-CANARY-{position}-0001");
    let mut out: Vec<(String, String)> = Vec::new();
    let mut add = |label: &str, text: String| out.push((label.to_owned(), text));
    let with_canary_env = |extra: &[(&str, Value)]| {
        let mut members: Vec<(&str, Value)> = vec![("env", json!({ "AWS_PROFILE": canary("ENV-VALUE") }))];
        members.extend(extra.iter().cloned());
        one(&members)
    };
    add("id", one(&[("id", json!(canary("ID")))]));
    add("id duplicate", file_of(vec![kube_doc(&[("name", json!(canary("NAME")))]), kube_doc(&[])]));
    add("tool", one(&[("tool", json!(canary("TOOL")))]));
    add("name long", one(&[("name", json!(format!("{}{}", canary("NAME"), rep('n', 70))))]));
    add("name control", one(&[("name", json!(format!("{}{}", canary("NAME"), ch(7))))]));
    add("context dash", one(&[("context", json!(format!("-{}", canary("CONTEXT"))))]));
    add("context hidden", one(&[("context", json!(format!("{}{}", canary("CONTEXT"), ch(0x202e))))]));
    add("namespace", one(&[("namespace", json!(canary("NAMESPACE")))]));
    add("subscription for kube", one(&[("subscription", json!(canary("SUBSCRIPTION")))]));
    add("section for kube", one(&[("section", json!(canary("SECTION")))]));
    add("node for kube", one(&[("node", json!(canary("NODE")))]));
    add("isolated kube", with_canary_env(&[("isolated", json!(true))]));
    add("production type", with_canary_env(&[("production", json!(canary("PRODUCTION")))]));
    add("isolated type", with_canary_env(&[("isolated", json!(canary("ISOLATED")))]));
    add("version", format!(r#"{{"version":"{}","profiles":[]}}"#, canary("VERSION")));
    add("unknown member", format!(r#"{{"version":1,"profiles":[],"{}":1}}"#, canary("MEMBER")));
    add("unknown profile member", with_canary_env(&[("HW-CANARY-FIELD", json!(canary("FIELD")))]));
    add("profiles as text", format!(r#"{{"version":1,"profiles":"{}"}}"#, canary("PROFILES")));
    add("profile as text", format!(r#"{{"version":1,"profiles":["{}"]}}"#, canary("PROFILE")));
    add("env name", one(&[("env", json!({ canary("NAME"): canary("ENV-VALUE") }))]));
    add("env name denied", one(&[("env", json!({ "LD_PRELOAD": canary("ENV-VALUE") }))]));
    add("env name windows case", one(&[("env", json!({ "Path": canary("ENV-VALUE") }))]));
    add("env name app set", one(&[("env", json!({ "KUBECONFIG": canary("ENV-VALUE") }))]));
    add("env name program", one(&[("env", json!({ "EDITOR": canary("ENV-VALUE") }))]));
    add("env value long", one(&[("env", json!({ "A": format!("{}{}", canary("ENV-VALUE"), rep('v', 1030)) }))]));
    add("env value control", one(&[("env", json!({ "A": format!("{}{}", canary("ENV-VALUE"), ch(1)) }))]));
    add("env value path", one(&[("env", json!({ "A": format!("/{}", canary("ENV-VALUE")) }))]));
    add("env value home path", one(&[("env", json!({ "A": format!("~/{}", canary("ENV-VALUE")) }))]));
    add("env value unc", one(&[("env", json!({ "A": format!("\\\\{}", canary("ENV-VALUE")) }))]));
    add("env value drive", one(&[("env", json!({ "A": format!("C:\\{}", canary("ENV-VALUE")) }))]));
    add("env value type", one(&[("env", json!({ "A": [canary("ENV-VALUE")] }))]));
    add("env as array", one(&[("env", json!([canary("ENV-VALUE")]))]));
    add("env too many", one(&[("env", Value::Object((0..=MAX_ENV).map(|i| (format!("V{i}"), json!(canary("ENV-VALUE")))).collect()))]));
    add("secret name", one(&[("secret_env", json!([canary("SECRET-NAME")]))]));
    add("secret name denied", one(&[("secret_env", json!(["LD_PRELOAD"])), ("env", json!({ "A": canary("ENV-VALUE") }))]));
    add("secret too many", one(&[("secret_env", Value::Array((0..=MAX_SECRET_NAMES).map(|i| json!(format!("S{i}"))).collect())), ("env", json!({ "A": canary("ENV-VALUE") }))]));
    add("secret duplicate", one(&[("secret_env", json!(["S", "S"])), ("env", json!({ "A": canary("ENV-VALUE") }))]));
    add("secret also plain", one(&[("secret_env", json!(["A"])), ("env", json!({ "A": canary("ENV-VALUE") }))]));
    add("secret as text", one(&[("secret_env", json!(canary("SECRET-NAME")))]));
    add("trailing text", format!("{} {}", one(&[]), canary("TRAILING")));
    add("two documents", format!("{}{}", one(&[]), one(&[("name", json!(canary("NAME")))])));
    out
}

/// A valid document with a canary in every value position, to mutate.
fn canary_base() -> String {
    one(&[
        ("name", json!("HW-CANARY-NAME-0001")),
        ("context", json!("HW-CANARY-CONTEXT-0001")),
        ("env", json!({ "AWS_PROFILE": "HW-CANARY-ENV-0001", "HTTPS_PROXY": "https://HW-CANARY-PROXY-0001.example.com" })),
        ("secret_env", json!(["AWS_SECRET_ACCESS_KEY"])),
    ])
}

#[test]
fn a_hostile_corpus_never_puts_a_value_into_a_note() {
    let documents = hostile_documents();
    assert!(documents.len() >= 40, "{} hostile documents", documents.len());
    for (label, text) in &documents {
        assert!(text.contains("CANARY"), "{label}: the document holds a canary");
        let note = refused(text);
        assert_no_canary(label, &note);
        // The same through the file: the quarantine note is the same sentence and the copy keeps the document.
        let dir = Scratch::new("profiles-hostile");
        fs::write(dir.path().join(FILE), text).expect("write");
        let (set, notes) = IntegrationProfiles::load(dir.path());
        assert!(set.items.is_empty(), "{label}: the app starts without profiles");
        assert_eq!(notes, vec![note.clone()], "{label}");
    }
    // The base is valid, so a mutation that stays valid is accepted and one that does not is refused without a value.
    accepted(&canary_base());
    let base = canary_base().into_bytes();
    let mut rng = Xorshift64::new(0x5eed_0008);
    let alphabet: &[u8] = b"\"{}[]:,\\/-_ aZ0\n\t";
    let mut accepted_n = 0;
    let mut refused_n = 0;
    for round in 0..400 {
        let mut text = base.clone();
        for _ in 0..=rng.below(3) {
            let at = rng.below(text.len());
            match rng.below(3) {
                0 => text[at] = alphabet[rng.below(alphabet.len())],
                1 => {
                    text.remove(at);
                }
                _ => text.insert(at, alphabet[rng.below(alphabet.len())]),
            }
        }
        let Ok(text) = String::from_utf8(text) else { continue };
        match loader_verdict(&text) {
            Ok(_) => accepted_n += 1,
            Err(note) => {
                refused_n += 1;
                assert_no_canary(&format!("mutation {round}"), &note);
            }
        }
    }
    assert!(accepted_n >= 10 && refused_n >= 10, "the mutations reach both verdicts: {accepted_n}, {refused_n}");
}

// ---------------------------------------------------------------------------------------------------------------------
// Secret variables: only in the keychain.
// ---------------------------------------------------------------------------------------------------------------------

fn env_name(text: &str) -> EnvName {
    EnvName::parse(text).expect("a valid variable name")
}

/// A set with one Kubernetes profile per id, each naming these secret variables.
fn set_with_secrets(ids: &[&str], names: &[&str]) -> IntegrationProfiles {
    let mut set = IntegrationProfiles::default();
    for profile_id in ids {
        let mut profile = IntegrationProfile::new(id(profile_id), ToolId::Kube, "A profile");
        profile.secret_env = names.iter().map(|name| (*name).to_owned()).collect();
        set.upsert(profile).expect("a valid profile");
    }
    set
}

/// A store whose every call fails with a message that holds a value, as a careless keychain binding might.
struct FailingStore;

const STORE_CANARY: &str = "HW-CANARY-STORE-0001";

impl SecretStore for FailingStore {
    fn get(&self, _account: &str) -> holzwave_core::Result<Option<String>> {
        Err(Error::Unsupported(format!("keychain: {STORE_CANARY}")))
    }

    fn set(&self, _account: &str, _secret: &str) -> holzwave_core::Result<()> {
        Err(Error::Unsupported(format!("keychain: {STORE_CANARY}")))
    }

    fn delete(&self, _account: &str) -> holzwave_core::Result<()> {
        Err(Error::Unsupported(format!("keychain: {STORE_CANARY}")))
    }
}

#[test]
fn a_secret_is_stored_in_the_store_under_its_account_and_is_capped() {
    assert_eq!(MAX_SECRET, 1024);
    assert!(MAX_SECRET < holzwave_core::secrets::MAX_LEN, "the core enforces the lower bound");
    let account = secret_account(&id("prod"), &env_name("AWS_SECRET_ACCESS_KEY"));
    assert_eq!(account, "int:prod:AWS_SECRET_ACCESS_KEY");

    let set = set_with_secrets(&["prod", "stage"], &["AWS_SECRET_ACCESS_KEY"]);
    let store = MemoryStore::default();
    let (prod, stage, name) = (id("prod"), id("stage"), env_name("AWS_SECRET_ACCESS_KEY"));
    assert!(!set.has_secret(&store, &prod, &name), "nothing is stored yet");

    // The cap is in bytes: 1024 fit, 1025 do not, and a refused value leaves the old one in place.
    set.set_secret(&store, &prod, &name, "first").expect("a small value");
    let at_the_cap = rep('k', MAX_SECRET);
    set.set_secret(&store, &prod, &name, &at_the_cap).expect("1024 bytes");
    assert_eq!(store.get(&account).expect("get"), Some(at_the_cap.clone()), "stored under its account");
    set.set_secret(&store, &prod, &name, &rep('k', MAX_SECRET + 1)).expect_err("1025 bytes");
    set.set_secret(&store, &prod, &name, &rep(ch(0xe9), MAX_SECRET / 2 + 1)).expect_err("1026 bytes of 513 characters");
    set.set_secret(&store, &prod, &name, &rep(ch(0xe9), MAX_SECRET / 2)).expect("1024 bytes of 512 characters");
    set.set_secret(&store, &prod, &name, &at_the_cap).expect("1024 bytes again");
    set.set_secret(&store, &prod, &name, "").expect_err("an empty value is not a secret");
    set.set_secret(&store, &prod, &name, &format!("a{}b", ch(0))).expect_err("a NUL");
    assert_eq!(store.get(&account).expect("get"), Some(at_the_cap.clone()), "a refused value changes nothing");

    // Only a name the profile lists, and only a profile that exists.
    set.set_secret(&store, &prod, &env_name("OTHER"), "x").expect_err("a name that is not in secret_env");
    assert_eq!(store.get("int:prod:OTHER").expect("get"), None);
    set.set_secret(&store, &id("ghost"), &name, "x").expect_err("a profile that does not exist");
    assert_eq!(store.get("int:ghost:AWS_SECRET_ACCESS_KEY").expect("get"), None);

    // Present and absent are a yes and a no; the same name under another profile is its own secret.
    assert!(set.has_secret(&store, &prod, &name));
    assert!(!set.has_secret(&store, &stage, &name));
    assert!(!set.has_secret(&store, &prod, &env_name("OTHER")), "a name that is not listed is a no");
    set.set_secret(&store, &stage, &name, "stage value").expect("the other profile");
    set.clear_secret(&store, &prod, &name).expect("clear");
    assert!(!set.has_secret(&store, &prod, &name));
    assert!(set.has_secret(&store, &stage, &name), "clearing one account leaves the other");
    set.clear_secret(&store, &prod, &name).expect("clearing what is not there is not an error");
    assert_eq!(store.get(&account).expect("get"), None);
}

#[test]
fn secrets_are_resolved_for_a_run_and_wiped_by_type() {
    let set = set_with_secrets(&["prod"], &["B_SECRET", "A_SECRET", "C_SECRET"]);
    let store = MemoryStore::default();
    let prod = id("prod");
    set.set_secret(&store, &prod, &env_name("A_SECRET"), "a-value").expect("a");
    set.set_secret(&store, &prod, &env_name("B_SECRET"), "b-value").expect("b");

    // The values come back as wiping strings, for the names that have one, in the order of the profile.
    let pairs: Vec<(String, Zeroizing<String>)> = set.resolve_secrets(&store, &prod).expect("resolve");
    let names: Vec<&str> = pairs.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["B_SECRET", "A_SECRET"], "the profile's order, and no entry for C_SECRET");
    assert_eq!(pairs[0].1.as_str(), "b-value");
    assert_eq!(pairs[1].1.as_str(), "a-value");

    // A profile without secret names has an empty list; a profile that does not exist is an error, not a silent run
    // without credentials.
    let plain = set_with_secrets(&["plain"], &[]);
    assert!(plain.resolve_secrets(&store, &id("plain")).expect("resolve").is_empty());
    assert!(set.resolve_secrets(&store, &id("ghost")).is_err());
    // A value left in the store for a name the profile no longer lists is not handed out.
    store.set("int:prod:D_SECRET", "stale").expect("set");
    assert_eq!(set.resolve_secrets(&store, &prod).expect("resolve").len(), 2);
    // A store that fails is an error, so a run never starts without the credentials the person asked for.
    assert!(set.resolve_secrets(&FailingStore, &prod).is_err());
}

#[test]
fn removing_a_profile_removes_its_secrets() {
    let mut set = set_with_secrets(&["prod", "stage"], &["TOKEN", "OTHER"]);
    let store = MemoryStore::default();
    let (prod, stage) = (id("prod"), id("stage"));
    for profile in [&prod, &stage] {
        for name in ["TOKEN", "OTHER"] {
            set.set_secret(&store, profile, &env_name(name), "v").expect("set");
        }
    }
    assert_eq!(set.remove_with_secrets(&store, &prod), Ok(true));
    assert!(set.get("prod").is_none() && set.get("stage").is_some());
    for name in ["TOKEN", "OTHER"] {
        assert_eq!(store.get(&format!("int:prod:{name}")).expect("get"), None, "{name} of the removed profile");
        assert_eq!(store.get(&format!("int:stage:{name}")).expect("get").as_deref(), Some("v"), "{name} of the other");
    }
    assert_eq!(set.remove_with_secrets(&store, &prod), Ok(false), "a profile that is not there");

    // A store that cannot delete keeps the profile, so that the next attempt finds the names again.
    let mut set = set_with_secrets(&["prod"], &["TOKEN"]);
    let note = set.remove_with_secrets(&FailingStore, &prod).expect_err("the keychain failed");
    assert!(set.get("prod").is_some(), "the profile is kept while its secrets could not be removed");
    assert!(!note.contains("CANARY") && note.contains("prod"), "{note}");
    // A profile without secret names is removed without asking the store.
    let mut plain = set_with_secrets(&["plain"], &[]);
    assert_eq!(plain.remove_with_secrets(&FailingStore, &id("plain")), Ok(true));
}

#[test]
fn no_secret_appears_in_a_file_a_debug_or_an_error() {
    let canary = "HW-CANARY-SECRET-0001";
    let mut set = set_with_secrets(&["prod"], &["AWS_SECRET_ACCESS_KEY"]);
    let store = MemoryStore::default();
    let (prod, name) = (id("prod"), env_name("AWS_SECRET_ACCESS_KEY"));
    set.set_secret(&store, &prod, &name, canary).expect("set");
    assert!(set.has_secret(&store, &prod, &name));

    // The files: the profile file and everything else in the directory.
    let dir = Scratch::new("profiles-secret-files");
    set.save(dir.path()).expect("save");
    for entry in fs::read_dir(dir.path()).expect("the directory") {
        let bytes = fs::read(entry.expect("an entry").path()).expect("a file");
        assert!(!String::from_utf8_lossy(&bytes).contains("CANARY"), "a file holds the secret value");
    }
    assert!(fs::read_to_string(dir.path().join(FILE)).expect("file").contains("AWS_SECRET_ACCESS_KEY"), "the name is kept");

    // Debug of the set, of a profile and of the account.
    for text in [format!("{set:?}"), format!("{set:#?}"), format!("{:?}", set.items[0]), format!("{:#?}", set.items[0])] {
        assert_no_canary("debug", &text);
    }
    // Errors: a refused value (the canary is inside it), a failing store, a missing profile.
    let long = format!("{canary}{}", rep('x', MAX_SECRET));
    let errors = [
        set.set_secret(&store, &prod, &name, &long).expect_err("too long"),
        set.set_secret(&store, &prod, &name, &format!("{canary}{}", ch(0))).expect_err("a NUL"),
        set.set_secret(&store, &prod, &env_name("UNLISTED"), canary).expect_err("an unlisted name"),
        set.set_secret(&store, &id("ghost"), &name, canary).expect_err("no such profile"),
        set.set_secret(&FailingStore, &prod, &name, canary).expect_err("the keychain failed"),
        set.clear_secret(&FailingStore, &prod, &name).expect_err("the keychain failed"),
        set.resolve_secrets(&FailingStore, &prod).expect_err("the keychain failed").to_string(),
        set.clone().remove_with_secrets(&FailingStore, &prod).expect_err("the keychain failed"),
    ];
    for (n, error) in errors.iter().enumerate() {
        assert_no_canary(&format!("error {n}"), error);
        assert!(!error.contains(STORE_CANARY) && !error.contains("keychain:"), "error {n} repeats the store: {error}");
        assert!(!error.is_empty() && error.len() < 200, "error {n} is one short sentence: {error}");
    }
    assert!(!set.has_secret(&FailingStore, &prod, &name), "a store that fails answers no");
    // The refused writes did not change the stored value.
    assert_eq!(store.get(&secret_account(&prod, &name)).expect("get").as_deref(), Some(canary));
    // Removing the profile wipes it, and the file written afterwards has no trace either.
    assert_eq!(set.remove_with_secrets(&store, &prod), Ok(true));
    assert_eq!(store.get(&secret_account(&prod, &name)).expect("get"), None);
    set.save(dir.path()).expect("save");
    assert!(!fs::read_to_string(dir.path().join(FILE)).expect("file").contains("CANARY"));
}
