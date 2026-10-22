//! The release gate for secrets: made-up canary values are put where a person's credentials live, and every output the
//! system can produce is searched for them.
//!
//! The canaries (none is a real credential; each has a shape nothing real has):
//!
//! - `HW-CANARY-SECRET-0001`: the value of a variable in a profile's `secret_env`, kept in the keychain;
//! - `HW-CANARY-ENV-0001`: the value of a variable in a profile's plain `env`;
//! - `HW-CANARY-TOKEN-0001` and `HW-CANARY-EXEC-ENV`: the token and the exec environment of a kubeconfig, which the
//!   fake prints unredacted (a real `kubectl config view` redacts them; the core must not depend on that);
//! - `HW-CANARY-STDERR-0001`: a line a tool writes to standard error.
//!
//! The secret may be in one place only: the environment of the child of the run that needed it (and of the pane of a
//! launch that needs it). The plain value is in that environment, in the profile as the dialog reads it and in the
//! profile file, which is where the person keeps it; it is in no other output. The kubeconfig canaries are in no
//! output at all. The standard error canary is in the sentence of the fault of the run that printed it, when it is
//! on the first line, and nowhere else.
//!
//! Nothing here uses a real tool, a network or a real credential. The tools are fake programs of the rig.
#![cfg(unix)]

mod common;
#[path = "common/fake_cli.rs"]
mod fake_cli;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use holzwave_core::integrate::args::{ContextName, Namespace, PodName, ProfileId, SubscriptionId};
use holzwave_core::integrate::azure::{login_state, LoginState};
use holzwave_core::integrate::context::ContextState;
use holzwave_core::integrate::kube::ContextEntry;
use holzwave_core::integrate::profiles::{IntegrationProfile, IntegrationProfiles, LocalFile};
use holzwave_core::integrate::run::{EnvExtra, EnvPolicy, Launch, RunEnd, RunError, RunResult, Runner};
use holzwave_core::integrate::service::{ChangeEvent, Done, FaultCode, HostFacts, IntegrationService};
use holzwave_core::integrate::{spec_of, Integration, Selection, ToolId};
use holzwave_core::platform::Os;
use holzwave_core::secrets::MemoryStore;
use serde_json::json;
use zeroize::Zeroizing;

use common::scratch_dir;
use fake_cli::FakeCli;

const SECRET: &str = "HW-CANARY-SECRET-0001";
const PLAIN: &str = "HW-CANARY-ENV-0001";
const TOKEN: &str = "HW-CANARY-TOKEN-0001";
const EXEC_ENV: &str = "HW-CANARY-EXEC-ENV";
const STDERR: &str = "HW-CANARY-STDERR-0001";
const ALL: [&str; 5] = [SECRET, PLAIN, TOKEN, EXEC_ENV, STDERR];

// ---- the scratch computer --------------------------------------------------------------------------------------------

struct Rig {
    root: PathBuf,
    bin: PathBuf,
    home: PathBuf,
    data: PathBuf,
    store: Arc<MemoryStore>,
}

impl Rig {
    fn new() -> Rig {
        let root = scratch_dir("canary");
        let (bin, home, data) = (root.join("bin"), root.join("home"), root.join("data"));
        for dir in [&bin, &home, &data] {
            std::fs::create_dir(dir).expect("a directory of the rig");
        }
        Rig { root, bin, home, data, store: Arc::new(MemoryStore::default()) }
    }

    fn facts(&self) -> HostFacts {
        HostFacts::for_tests(&self.root, std::slice::from_ref(&self.bin), &self.home, &self.data)
    }

    fn service_over(&self, facts: HostFacts) -> IntegrationService {
        IntegrationService::new(&self.data, self.store.clone(), facts, &[])
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fixture(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integrations").join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("fixture {relative}: {e}"))
}

fn args(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

fn argv_of(fake: &FakeCli) -> Vec<String> {
    fake.argv().into_iter().map(|a| String::from_utf8_lossy(&a).into_owned()).collect()
}

/// The kubeconfig of the fixture, as `kubectl config view -o json` would print it if it did not redact: the token and
/// the exec environment are there.
fn unredacted_config() -> String {
    let mut config: serde_json::Value = serde_json::from_str(&fixture("kubectl/config_view.json")).expect("the recorded config");
    config["users"] = json!([
        { "name": "token-user", "user": { "token": TOKEN } },
        { "name": "exec-user", "user": { "exec": {
            "apiVersion": "client.authentication.k8s.io/v1beta1",
            "command": "aws",
            "args": ["eks", "get-token", "--cluster-name", "demo"],
            "env": [{ "name": "AWS_PROFILE", "value": EXEC_ENV }],
            "interactiveMode": "IfAvailable"
        } } }
    ]);
    config.to_string()
}

fn profile() -> IntegrationProfile {
    let mut profile = IntegrationProfile::new(ProfileId::parse("aws-prod").expect("an id"), ToolId::Kube, "AWS prod");
    profile.env.insert("AWS_PROFILE".to_owned(), PLAIN.to_owned());
    profile.secret_env.push("AWS_SECRET_ACCESS_KEY".to_owned());
    profile
}

/// Everything the service gave out, with the canaries each output may hold. A canary found anywhere else is a leak.
#[derive(Default)]
struct Sweep {
    outputs: Vec<(String, String, Vec<&'static str>)>,
}

impl Sweep {
    fn add(&mut self, what: &str, text: impl Into<String>) {
        self.outputs.push((what.to_owned(), text.into(), Vec::new()));
    }

    fn add_allowing(&mut self, what: &str, text: impl Into<String>, may_hold: &[&'static str]) {
        self.outputs.push((what.to_owned(), text.into(), may_hold.to_vec()));
    }

    fn json<T: serde::Serialize>(&mut self, what: &str, value: &T, may_hold: &[&'static str]) {
        self.add_allowing(what, serde_json::to_string(value).expect("JSON"), may_hold);
    }

    fn check(&self) -> usize {
        for (what, text, may_hold) in &self.outputs {
            for canary in ALL {
                assert!(!text.contains(canary) || may_hold.contains(&canary), "the canary {canary} is in {what}: {}", cut(text));
            }
        }
        self.outputs.len()
    }
}

fn cut(text: &str) -> String {
    text.chars().take(300).collect()
}

/// Every file below `dir`, except below `skip`: (path, bytes as text).
fn files_below(dir: &Path, skip: &[&Path]) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if skip.iter().any(|s| dir.starts_with(s)) {
            continue;
        }
        for entry in std::fs::read_dir(&dir).expect("a directory").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push((path.clone(), String::from_utf8_lossy(&std::fs::read(&path).expect("a file")).into_owned()));
            }
        }
    }
    out
}

// ---- the sweep ---------------------------------------------------------------------------------------------------------

#[test]
fn the_canary_is_only_in_the_child_environment() {
    let rig = Rig::new();
    let events: Arc<Mutex<Vec<ChangeEvent>>> = Arc::default();
    let facts = rig.facts();
    let facts_debug = format!("{facts:?}");
    let service = rig.service_over(rig.facts()).shared();
    let sink = events.clone();
    service.set_on_change(Arc::new(move |event| sink.lock().expect("the log").push(event)));
    let mut sweep = Sweep::default();
    let mut argument_lists: Vec<String> = Vec::new();

    // The person's kubeconfig holds the token and the exec environment; the tool prints it unredacted.
    let personal = rig.home.join(".kube").join("config");
    std::fs::create_dir_all(personal.parent().expect("a parent")).expect("a directory");
    std::fs::write(&personal, fixture("kubectl/config_view_canary.kubeconfig")).expect("the person's kubeconfig");
    let shell = holzwave_core::platform::ShellLaunch { program: "/bin/zsh".to_owned(), args: vec!["-l".to_owned()] };
    let service_with_shell = rig.service_over(rig.facts().with_shell(shell)).shared();
    let config = FakeCli::new(&rig.bin, "kubectl").prints(&unredacted_config());

    // 1. The kubeconfig canaries: the list of contexts, the tool's own selection, the status rows and the overlay of a shell pane.
    for service in [&service, &service_with_shell] {
        let contexts = service.run_view("kube", "contexts", Some(7), &args(&[])).expect("the contexts");
        sweep.add("the contexts answer", format!("{contexts:?}"));
        sweep.json("the contexts answer as JSON", &contexts, &[]);
        argument_lists.push(argv_of(&config).join(" "));
        let selected = service.select("kube", Some(7), &Selection { context: Some("kind-test".into()), namespace: Some("payments".into()), ..Selection::default() });
        sweep.add("the selection", format!("{selected:?}"));
        service.context_rows(Some(7), true);
        assert!(service.wait_idle(std::time::Duration::from_secs(30)));
        sweep.add("the context rows", format!("{:?}", service.context_rows(Some(7), false)));
        sweep.add("the tools", format!("{:?}", service.tools()));
        sweep.add("the pane rows", format!("{:?}", service.pane_rows()));
        sweep.add("the notes", service.notes().join("\n"));
    }
    let launch = service_with_shell.launch("kube", "shell", Some(7), &args(&[])).expect("the shell launches");
    sweep.add("the shell launch", format!("{launch:?} {:?} {:?} {:?}", launch.program, launch.args, launch.env));
    let overlay = rig.data.join("run").join("kube").join("7.json");
    let overlay_text = std::fs::read_to_string(&overlay).expect("the overlay exists");
    assert!(overlay_text.contains("kind-test"), "the overlay names the context: {overlay_text}");
    sweep.add("the overlay of the shell pane", overlay_text);

    // 2. The secret and the plain value of a profile, in the runs that need them and in the runs that do not.
    let pods = FakeCli::new(&rig.bin, "kubectl").prints(&fixture("kubectl/pods.json"));
    service.save_profile(profile()).expect("a profile");
    service.set_secret("aws-prod", "AWS_SECRET_ACCESS_KEY", SECRET).expect("a value in the keychain");
    let with_profile = Selection { profile: Some("aws-prod".into()), ..Selection::default() };
    service.select("kube", Some(3), &with_profile).expect("a selection with the profile");

    let env_has = |fake: &FakeCli, name: &str, value: &str| fake.env().get(name).map(String::as_str) == Some(value);
    let mut secret_runs = 0;
    let mut plain_runs = 0;

    let read = service.run_view("kube", "pods", Some(3), &args(&[])).expect("a read with the profile");
    assert!(env_has(&pods, "AWS_SECRET_ACCESS_KEY", SECRET), "the child of the run that needed the secret has it");
    assert!(env_has(&pods, "AWS_PROFILE", PLAIN), "and the plain value");
    (secret_runs, plain_runs) = (secret_runs + 1, plain_runs + 1);
    sweep.add("the read with the profile", format!("{read:?}"));
    sweep.json("the read with the profile as JSON", &read, &[]);
    argument_lists.push(argv_of(&pods).join(" "));

    let other = service.run_view("kube", "namespaces", Some(4), &args(&[])).expect("a read in a pane without the profile");
    let env = pods.env();
    assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY") && !env.contains_key("AWS_PROFILE"), "the next run, without the profile, has neither: {:?}", env.keys().collect::<Vec<_>>());
    assert!(!env.values().any(|v| ALL.iter().any(|c| v.contains(c))), "no variable of that child holds a canary");
    sweep.add("the read without the profile", format!("{other:?}"));
    argument_lists.push(argv_of(&pods).join(" "));

    // A card and its confirmation: the run that needed the secret, and one that did not.
    let card = service.prepare("kube", "delete_pod", Some(3), &args(&[("pod", "api-0")])).expect("a card");
    sweep.add("the card", format!("{card:?}"));
    sweep.json("the card as JSON", &card, &[]);
    assert_eq!(argv_of(&pods).join(" "), argument_lists.last().cloned().expect("a record"), "preparing a card started nothing: the record is still the last run's");
    let done = service.confirm(&card.ticket, &card.hash).expect("the confirmed card");
    assert!(matches!(done, Done::Ran { .. }), "{done:?}");
    assert!(env_has(&pods, "AWS_SECRET_ACCESS_KEY", SECRET) && env_has(&pods, "AWS_PROFILE", PLAIN), "the confirmed run has the secret and the plain value");
    (secret_runs, plain_runs) = (secret_runs + 1, plain_runs + 1);
    sweep.add("the confirmed card", format!("{done:?}"));
    argument_lists.push(argv_of(&pods).join(" "));
    let card = service.prepare("kube", "delete_pod", Some(4), &args(&[("pod", "api-1")])).expect("a card without the profile");
    service.confirm(&card.ticket, &card.hash).expect("the confirmed card without the profile");
    assert!(!pods.env().contains_key("AWS_SECRET_ACCESS_KEY") && !pods.env().contains_key("AWS_PROFILE"), "a run without the profile has neither");
    argument_lists.push(argv_of(&pods).join(" "));

    // A launch: the pane gets the plain value in its variables and the secret in its own list, for the profile's pane only.
    let logs = service.launch("kube", "logs", Some(3), &args(&[("pod", "api-0")])).expect("a launch with the profile");
    assert_eq!(logs.secrets.len(), 1, "the pane of the profile gets the secret");
    assert!(logs.env.iter().any(|(name, value)| name == "AWS_PROFILE" && value == PLAIN), "and the plain value");
    assert!(!logs.env.iter().any(|(_, value)| value.contains(SECRET)), "the secret is not among the plain variables");
    sweep.add_allowing("the launch with the profile (variables)", format!("{:?} {:?}", logs.env, logs.args), &[PLAIN]);
    sweep.add("the launch with the profile (Debug)", format!("{logs:?}"));
    let bare = service.launch("kube", "logs", Some(4), &args(&[("pod", "api-0")])).expect("a launch without the profile");
    assert!(bare.secrets.is_empty() && !bare.env.iter().any(|(name, _)| name == "AWS_PROFILE"), "a pane without the profile gets neither");
    sweep.add("the launch without the profile", format!("{bare:?} {:?} {:?}", bare.env, bare.args));
    let exec = service.prepare("kube", "exec", Some(3), &args(&[("pod", "api-0")])).expect("a card for a session");
    let Done::Launch(session) = service.confirm(&exec.ticket, &exec.hash).expect("the session") else { panic!("an exec verb launches") };
    assert_eq!(session.secrets.len(), 1, "the session gets the secret in its own list");
    sweep.add("the session launch (Debug)", format!("{session:?}"));
    sweep.json("the card of the session", &exec, &[]);
    (secret_runs, plain_runs) = (secret_runs + 1, plain_runs + 1);

    // A failing run: the standard error canary on the first line is in the sentence of the fault, once, and nowhere else.
    let failing = FakeCli::new(&rig.bin, "kubectl").prints_stderr(&format!("error: {STDERR}\nsecond line: {STDERR}\n")).exits(1);
    let fault = service.run_view("kube", "pods", Some(3), &args(&[])).expect_err("a failure");
    assert_eq!(fault.code, FaultCode::Failed);
    assert_eq!(fault.text.matches(STDERR).count(), 1, "the first line is in the sentence, the second is not: {}", fault.text);
    argument_lists.push(argv_of(&failing).join(" "));
    sweep.add("the Debug of the fault", format!("{fault:?}"));
    sweep.add_allowing("the sentence of the fault", format!("{fault} {}", fault.text), &[STDERR]);
    sweep.json("the fault as JSON", &fault, &[STDERR]);
    let hidden = FakeCli::new(&rig.bin, "kubectl").prints_stderr(&format!("Error from server\ndetail: {STDERR}\n")).exits(1);
    let fault = service.run_view("kube", "pods", Some(3), &args(&[])).expect_err("a failure");
    assert!(!fault.text.contains(STDERR), "a line after the first never reaches the sentence: {}", fault.text);
    sweep.add("the fault of a later line", format!("{fault:?} {fault} {}", fault.text));
    argument_lists.push(argv_of(&hidden).join(" "));

    // The profile API, the tools, the service and the facts.
    let profiles = service.profiles();
    let view = &profiles.profiles[0];
    assert_eq!(view.env.get("AWS_PROFILE").map(String::as_str), Some(PLAIN), "the dialog reads the plain value: it edits it");
    assert_eq!((view.secret_env[0].name.as_str(), view.secret_env[0].has_value), ("AWS_SECRET_ACCESS_KEY", true));
    sweep.add_allowing("the profiles", format!("{profiles:?}"), &[PLAIN]);
    sweep.json("the profiles as JSON", &profiles, &[PLAIN]);
    let saved = service.save_profile(profile()).expect("a save");
    sweep.json("the answer of a save", &saved, &[PLAIN]);
    sweep.add("the tools", format!("{:?}", service.tools()));
    sweep.add("the service", format!("{service:?}"));
    sweep.add("the host facts", facts_debug);
    sweep.add("the notes", service.notes().join("\n"));
    sweep.add("the pane rows", format!("{:?}", service.pane_rows()));
    sweep.add("the context rows", format!("{:?}", service.context_rows(Some(3), false)));
    assert!(service.wait_idle(std::time::Duration::from_secs(30)));
    sweep.add("the events", format!("{:?}", events.lock().expect("the log")));
    for (i, list) in argument_lists.iter().enumerate() {
        sweep.add(&format!("the argument list recorded at step {i}"), list.clone());
    }

    // The files: the secret is in none; the plain value is in the profile file and in no other; the rest is in none.
    let bin = rig.bin.clone();
    let home = rig.home.clone();
    let mut files = 0;
    for (path, text) in files_below(&rig.root, &[bin.as_path(), home.as_path()]) {
        files += 1;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let may: &[&'static str] = if name == "integrations.json" { &[PLAIN] } else { &[] };
        sweep.add_allowing(&format!("the file {}", path.display()), text, may);
    }
    assert!(files >= 3, "the data directory holds the profile file, the pane file and the overlay: {files}");
    let profile_file = std::fs::read_to_string(rig.data.join("integrations.json")).expect("the profile file");
    assert!(profile_file.contains(PLAIN) && !profile_file.contains(SECRET), "the plain value is in the profile file, the secret is not");

    let checked = sweep.check();
    assert!(secret_runs >= 3 && plain_runs >= 3, "the secret and the plain value were seen in {secret_runs} and {plain_runs} child environments");
    println!(
        "Canary: {checked} outputs checked for {} canaries ({files} files, {} argument lists, answers, models, faults, cards, launches, profiles, tools, notes, events); \
         the secret was in the environment of {secret_runs} runs and in no other output, the plain value in {plain_runs} runs, the dialog's read and the profile file",
        ALL.len(),
        argument_lists.len()
    );
}

// ---- the environment policy ---------------------------------------------------------------------------------------

const DENIED: [&str; 10] = [
    "HOLZWAVE_IPC",
    "HOLZWAVE_TOKEN",
    "LD_PRELOAD",
    "DYLD_INSERT_LIBRARIES",
    "PYTHONSTARTUP",
    "NODE_OPTIONS",
    "GIT_SSH_COMMAND",
    "BASH_ENV",
    "ENV",
    "IFS",
];
const CREDENTIALS: [(&str, &str); 3] = [
    ("AWS_SECRET_ACCESS_KEY", "aws-credential"),
    ("AZURE_CLIENT_SECRET", "azure-credential"),
    ("GOOGLE_APPLICATION_CREDENTIALS", "/home/person/gcp.json"),
];

#[test]
fn the_environment_policy_holds_end_to_end() {
    let rig = Rig::new();
    let mut dirty = rig.facts();
    // A second PATH in the parent environment, before the real one (the detector reads the last): the child gets neither.
    dirty.inherited.insert(0, (OsString::from("PATH"), OsString::from("/poisoned/bin")));
    for name in DENIED {
        dirty = dirty.with_env(name, "from-the-parent");
    }
    for (name, value) in CREDENTIALS {
        dirty = dirty.with_env(name, value);
    }
    let service = rig.service_over(dirty);
    let (mut children, mut checks) = (0, 0);
    for (tool, program) in [(ToolId::Kube, "kubectl"), (ToolId::Talos, "talosctl"), (ToolId::Azure, "az"), (ToolId::Gam, "gam")] {
        let fake = FakeCli::new(&rig.bin, program).prints("{}\n");
        let first = spec_of(tool).verbs.first().expect("a verb");
        let _ = service.run_view(tool.as_str(), first.id, Some(1), &args(&[]));
        let env = fake.env();
        children += 1;
        for name in DENIED {
            assert!(!env.contains_key(name), "{program}: the child has the denied name {name}");
            checks += 1;
        }
        for (name, value) in CREDENTIALS {
            assert_eq!(env.get(name).map(String::as_str), Some(value), "{program}: the credential {name} reaches the tool, which needs it");
            checks += 1;
        }
        let path = env.get("PATH").expect("the child has a PATH");
        assert!(!path.contains("poisoned"), "{program}: the PATH is the rebuilt one: {path}");
        assert!(path.split(':').any(|dir| Path::new(dir) == rig.bin), "{program}: the rebuilt PATH has the tool's own directory: {path}");
        assert!(!env.keys().any(|name| name.starts_with("HOLZWAVE_") || name.starts_with("LD_") || name.starts_with("DYLD_")), "{program}: no loader or app variable");
    }

    // The launch of a pane inherits by itself: it carries the names to remove, the denied ones that are present, and not the credentials.
    let launch = service.launch("kube", "logs", Some(1), &args(&[("pod", "api-0")])).expect("a launch");
    let mut expected: Vec<String> = DENIED.iter().map(|name| (*name).to_owned()).collect();
    expected.sort();
    assert_eq!(launch.env_remove, expected, "the launch lists the denied names that were present and not PATH");
    assert!(!launch.env_remove.iter().any(|name| CREDENTIALS.iter().any(|(own, _)| own == name)), "no credential is removed");
    assert!(launch.env.iter().all(|(name, _)| !DENIED.contains(&name.as_str())), "and sets none of them");
    assert!(launch.env.iter().any(|(name, value)| name == "PATH" && !value.contains("poisoned")), "the launch sets the rebuilt PATH");
    let removals = launch.env_remove.len();

    // Windows: the rule is the same without regard to case, and the key of a variable is its upper-case name.
    let inherited = |names: &[&str]| -> Vec<(OsString, OsString)> { names.iter().map(|name| (OsString::from(*name), OsString::from("v"))).collect() };
    let names = [
        "ld_preload", "Ld_Preload", "holzwave_token", "HolzWave_Ipc", "dyld_insert_libraries", "Pythonstartup", "node_options", "git_ssh_command", "Bash_Env", "env",
        "ifs", "Path", "aws_secret_access_key", "Azure_Client_Secret",
    ];
    let windows = EnvPolicy::build(inherited(&names), Os::Windows, "C:\\rebuilt".to_owned(), EnvExtra::default()).expect("an environment");
    let kept: Vec<String> = windows.names().into_iter().map(str::to_owned).collect();
    assert_eq!(kept, ["Azure_Client_Secret", "PATH", "aws_secret_access_key"], "on Windows only the credentials and the rebuilt PATH stay: {kept:?}");
    assert_eq!(windows.value("PATH"), Some("C:\\rebuilt"), "and the PATH is the rebuilt one, whatever the case of the inherited one");
    let unix = EnvPolicy::build(inherited(&names), Os::Linux, "/rebuilt".to_owned(), EnvExtra::default()).expect("an environment");
    let kept: Vec<&str> = unix.names();
    assert!(kept.contains(&"ld_preload") && kept.contains(&"Path") && kept.contains(&"env"), "on Unix the names are case-sensitive: {kept:?}");
    assert!(!kept.iter().any(|name| matches!(*name, "BASH_ENV" | "IFS" | "LD_PRELOAD")), "{kept:?}");
    let removals_windows = EnvPolicy::removals(names.iter().map(OsString::from), Os::Windows);
    assert_eq!(
        removals_windows,
        ["Bash_Env", "HolzWave_Ipc", "Ld_Preload", "Pythonstartup", "dyld_insert_libraries", "env", "git_ssh_command", "holzwave_token", "ifs", "ld_preload", "node_options"],
        "a launch on Windows removes every denied name in whatever case it has, and not PATH"
    );
    let windows_launch = Launch::new(
        PathBuf::from("C:\\tools\\kubectl.exe"),
        vec![OsString::from("logs")],
        EnvPolicy::build(Vec::<(OsString, OsString)>::new(), Os::Windows, "C:\\rebuilt".to_owned(), EnvExtra::default()).expect("an environment"),
        names.iter().map(OsString::from),
        Os::Windows,
        None,
        "logs".to_owned(),
    );
    assert_eq!(windows_launch.env_remove, removals_windows);
    // A name the profile sets may not be denied in any case, or be one the app sets from its own state.
    for denied in ["ld_preload", "Dyld_Library_Path", "Node_Options", "path", "bash_env"] {
        let extra = EnvExtra { plain: vec![(denied.to_owned(), "x".to_owned())], ..EnvExtra::default() };
        assert!(EnvPolicy::build(Vec::<(OsString, OsString)>::new(), Os::Windows, "C:\\p".to_owned(), extra).is_err(), "{denied} is refused on Windows");
    }
    for app_set in ["KUBECONFIG", "TALOSCONFIG", "AZURE_CONFIG_DIR", "GAMCFGDIR", "GAMCFGSECTION"] {
        let extra = EnvExtra { plain: vec![(app_set.to_owned(), "x".to_owned())], ..EnvExtra::default() };
        assert!(EnvPolicy::build(Vec::<(OsString, OsString)>::new(), Os::Linux, "/p".to_owned(), extra).is_err(), "a profile may not set {app_set}");
    }

    // A synced profile that sets a denied name, an app-set name or a denied secret name is quarantined whole: nothing of
    // the file is used, the other profile in it too, the note is one sentence without a value, and no process starts.
    let mut quarantined = 0;
    for (member, value) in [
        ("env", json!({ "LD_PRELOAD": PLAIN })),
        ("env", json!({ "Ld_Preload": PLAIN })),
        ("env", json!({ "HOLZWAVE_TOKEN": PLAIN })),
        ("env", json!({ "KUBECONFIG": PLAIN })),
        ("env", json!({ "PATH": PLAIN })),
        ("secret_env", json!(["LD_PRELOAD"])),
        ("secret_env", json!(["KUBECONFIG"])),
    ] {
        let rig = Rig::new();
        let fake = FakeCli::new(&rig.bin, "kubectl").prints("{}\n");
        let good = json!({ "id": "good", "tool": "kube", "name": "Good" });
        let bad = json!({ "id": "bad", "tool": "kube", "name": "Bad", member: value });
        std::fs::write(rig.data.join("integrations.json"), json!({ "version": 1, "profiles": [good, bad] }).to_string()).expect("the synced file");
        let service = rig.service_over(rig.facts());
        let view = service.profiles();
        assert!(view.profiles.is_empty(), "{member} {value}: the good profile of the same file is not used either");
        assert_eq!(view.notes.len(), 1, "{member} {value}: one note: {:?}", view.notes);
        for note in view.notes.iter().chain(service.notes().iter()) {
            assert!(!note.contains(PLAIN), "{member} {value}: the note holds a value: {note}");
        }
        let selected = service.select("kube", Some(2), &Selection { profile: Some("good".into()), ..Selection::default() });
        let refusal = selected.expect_err("the profile of the quarantined file is unknown");
        assert_eq!(refusal.code, FaultCode::Refused, "{member} {value}: {refusal:?}");
        assert!(std::fs::metadata(rig.data.join("integrations.json.bad")).is_ok(), "{member} {value}: the file is kept as it was");
        assert!(!fake.started(), "{member} {value}: nothing was started");
        quarantined += 1;
    }
    println!(
        "Env policy: {children} children of the four tools lacked all {} denied names ({checks} checks) and had the {} credential names and the rebuilt PATH; the launch lists {removals} removals; \
         Windows case rule (build, removals, launch, profile names), {quarantined} synced profiles quarantined whole",
        DENIED.len(),
        CREDENTIALS.len()
    );
}

// ---- Debug --------------------------------------------------------------------------------------------------------

#[test]
fn the_debug_of_every_public_type_prints_counts_and_ids_only() {
    const NAME: &str = "HW-CANARY-NAME-0001";
    let rig = Rig::new();
    let mut shown: Vec<(&str, String)> = Vec::new();

    // The environment and what builds it.
    let extra = EnvExtra {
        plain: vec![("A".into(), PLAIN.into())],
        fixed: &[("FIXED", "fixed")],
        isolation: vec![("ISO".into(), format!("/{NAME}"))],
        secrets: vec![("B".into(), Zeroizing::new(SECRET.to_owned()))],
    };
    shown.push(("EnvExtra", format!("{extra:?}")));
    let env = EnvPolicy::build(vec![(OsString::from("INHERITED"), OsString::from(PLAIN))], Os::Linux, format!("/{NAME}"), extra).expect("an environment");
    shown.push(("Env", format!("{env:?}")));
    let launch = Launch::new(
        PathBuf::from(format!("/{NAME}/tool")),
        vec![OsString::from(NAME)],
        env,
        [OsString::from("LD_PRELOAD")],
        Os::Linux,
        Some(PathBuf::from(format!("/{NAME}"))),
        NAME.to_owned(),
    );
    shown.push(("Launch", format!("{launch:?}")));
    let result = RunResult { stdout: SECRET.as_bytes().to_vec(), stderr: STDERR.as_bytes().to_vec(), status: Some(1), end: RunEnd::Finished, stdout_dropped: false };
    shown.push(("RunResult", format!("{result:?}")));
    shown.push(("RunError", format!("{:?} {:?}", RunError::EnvName, RunError::EnvValue)));
    shown.push(("Runner", format!("{:?}", Runner::new(true))));

    // The errors of the refusal of a value: no variant carries text.
    for hostile in [format!("-{NAME}"), format!("{NAME}\n"), format!("{NAME} {NAME}"), NAME.to_lowercase(), format!("{NAME}\u{202e}")] {
        shown.push(("ArgError", format!("{:?} {:?} {:?}", ContextName::parse(&hostile).err(), Namespace::parse(&hostile).err(), PodName::parse(&hostile).err())));
        shown.push(("ArgError (subscription)", format!("{:?}", SubscriptionId::parse(&hostile).err())));
        shown.push(("ArgError (profile)", format!("{:?}", ProfileId::parse(&hostile).err())));
    }
    // The names: a valid one prints its length.
    shown.push(("ContextName", format!("{:?}", ContextName::parse(NAME).expect("a context"))));
    shown.push(("ContextEntry", format!("{:?}", ContextEntry::new(NAME, NAME, NAME, Some(NAME)))));
    shown.push(("ProfileId", format!("{:?}", ProfileId::parse("canary").expect("an id"))));

    // The profiles and the local file.
    let mut one = profile();
    one.name = NAME.to_owned();
    one.context = Some(ContextName::parse(NAME).expect("a context"));
    shown.push(("IntegrationProfile", format!("{one:?}")));
    let mut set = IntegrationProfiles::default();
    set.upsert(one.clone()).expect("a profile");
    shown.push(("IntegrationProfiles", format!("{set:?}")));
    let mut local = LocalFile::default();
    local.set_config_paths(ToolId::Kube, &[PathBuf::from(format!("/{NAME}/config"))]).expect("a path list");
    shown.push(("LocalFile", format!("{local:?}")));
    let parse_error = IntegrationProfiles::parse(&json!({ "version": 1, "profiles": [{ "id": "p", "tool": "kube", "name": "n", "env": { "LD_PRELOAD": PLAIN } }] }).to_string()).expect_err("denied");
    shown.push(("the note of a refused file", parse_error));
    let parse_error = IntegrationProfiles::parse(&format!("{{\"version\":1,\"profiles\":[{{\"id\":\"{SECRET}\"}}]}}")).expect_err("refused");
    shown.push(("the note of a damaged file", parse_error));

    // The parsers: a tool's output that is no answer is a fixed sentence, and a sign-in state prints sizes.
    for (tool, tool_ref) in [(ToolId::Kube, &holzwave_core::integrate::kube::KUBE as &dyn Integration), (ToolId::Talos, &holzwave_core::integrate::talos::TALOS), (ToolId::Azure, &holzwave_core::integrate::azure::AZURE), (ToolId::Gam, &holzwave_core::integrate::gam::GAM)] {
        for verb in spec_of(tool).verbs {
            if let Err(error) = tool_ref.view(verb, format!("{{\"{SECRET}\": [{TOKEN}").as_bytes()) {
                shown.push(("ParseError", format!("{error:?} {error}")));
            }
        }
    }
    let state = login_state(json!({ "user": { "name": NAME }, "id": "11111111-2222-4333-8444-555555555555", "name": TOKEN }).to_string().as_bytes());
    assert!(matches!(state, LoginState::SignedIn { .. }), "a signed-in account: {state:?}");
    shown.push(("LoginState", format!("{state:?}")));

    // The context state and the service with every kind of value in it.
    let state = ContextState::new();
    shown.push(("ContextState", format!("{state:?}")));
    let facts = rig.facts().with_env("SECRET_IN_THE_PARENT", SECRET).with_env("TOKEN_IN_THE_PARENT", TOKEN);
    shown.push(("HostFacts", format!("{facts:?}")));
    let service = rig.service_over(facts);
    service.save_profile(profile()).expect("a profile");
    service.set_secret("aws-prod", "AWS_SECRET_ACCESS_KEY", SECRET).expect("a secret");
    shown.push(("IntegrationService", format!("{service:?}")));
    let fault = service.run_view("kube", "pods", None, &args(&[("pod", SECRET)])).expect_err("refused");
    shown.push(("Fault", format!("{fault:?}")));
    let fault = service.confirm(&SECRET.to_lowercase(), &"0".repeat(64)).expect_err("refused");
    shown.push(("Fault (ticket)", format!("{fault:?} {fault}")));

    let mut checked = 0;
    for (what, text) in &shown {
        for canary in ALL.iter().copied().chain([NAME]) {
            assert!(!text.contains(canary), "the Debug or note of {what} holds {canary}: {}", cut(text));
        }
        checked += 1;
    }
    // The canaries are there to be found: the same texts built by hand hold them, so the check above can fail.
    let control = format!("{SECRET} {NAME}");
    assert!(ALL.iter().chain([&NAME]).any(|canary| control.contains(canary)));
    println!("Debug: {checked} Debug outputs and notes of {} kinds of value hold none of the {} canaries", shown.len(), ALL.len() + 1);
}
