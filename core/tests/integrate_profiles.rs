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

use holzwave_core::integrate::args::{ContextName, ProfileId};
use holzwave_core::integrate::profiles::{IntegrationProfile, IntegrationProfiles, FILE};
use holzwave_core::integrate::ToolId;
use serde_json::{json, Value};

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
