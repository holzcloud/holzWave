//! The approval pipeline for the verbs that change something: a card, a hash that binds it to the request, a confirm that
//! resolves the request again, and an approval that only that confirm can make.
//!
//! The tests run against a toy resolver that stands in for the service. Hostile names are built from code points, never
//! typed, so that this file holds no hidden character itself.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::Mutex;

use holzwave_core::integrate::ticket::{Resolution, Resolve, TicketError, TicketTable};
use holzwave_core::integrate::{ArgKind, ArgSpec, Output, ToolId, Verb, VerbEffect};

static POD_ARG: [ArgSpec; 1] = [ArgSpec { name: "pod", kind: ArgKind::Pod, required: true }];

static DELETE_POD: Verb = Verb {
    id: "delete_pod",
    label: "Delete pod",
    effect: VerbEffect::Write,
    output: Output::Done,
    args: &POD_ARG,
    refresh_secs: None,
    exit_is_state: false,
};

static PODS: Verb = Verb {
    id: "pods",
    label: "Pods",
    effect: VerbEffect::Read,
    output: Output::View,
    args: &[],
    refresh_secs: Some(5),
    exit_is_state: false,
};

/// What the toy resolver sees of the world. A test changes it between prepare and confirm.
struct World {
    context: String,
    namespace: String,
}

/// A toy that resolves `kube` verbs the way the service will: from the request and from the selection it holds.
struct Toy {
    world: Mutex<World>,
}

impl Toy {
    fn new() -> Self {
        Self { world: Mutex::new(World { context: "prod".to_owned(), namespace: "payments".to_owned() }) }
    }
}

impl Resolve for Toy {
    fn resolve(
        &self,
        tool: ToolId,
        verb: &str,
        _pane: Option<u64>,
        args: &BTreeMap<String, String>,
    ) -> Result<Resolution, TicketError> {
        assert_eq!(tool, ToolId::Kube, "the toy only knows kube");
        let world = self.world.lock().unwrap();
        match verb {
            "delete_pod" => {
                let pod = args.get("pod").ok_or(TicketError::Refused)?;
                Ok(Resolution {
                    verb: &DELETE_POD,
                    argv: ["delete", "pod"]
                        .iter()
                        .map(OsString::from)
                        .chain([
                            OsString::from(format!("--context={}", world.context)),
                            OsString::from(format!("--namespace={}", world.namespace)),
                            OsString::from("--wait=false"),
                            OsString::from("--"),
                            OsString::from(pod),
                        ])
                        .collect(),
                    program: "kubectl".to_owned(),
                    target: vec![
                        ("context".to_owned(), world.context.clone()),
                        ("namespace".to_owned(), world.namespace.clone()),
                    ],
                    production: None,
                })
            }
            "pods" => Ok(Resolution {
                verb: &PODS,
                argv: vec![OsString::from("get"), OsString::from("pods")],
                program: "kubectl".to_owned(),
                target: Vec::new(),
                production: None,
            }),
            _ => Err(TicketError::Refused),
        }
    }
}

fn pod(name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("pod".to_owned(), name.to_owned())])
}

fn is_lower_hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[test]
fn a_gated_verb_is_prepared_and_confirmed_once() {
    let toy = Toy::new();
    let table = TicketTable::new();
    let card = table.prepare(&toy, ToolId::Kube, "delete_pod", Some(3), &pod("api-0")).unwrap();
    assert!(is_lower_hex(&card.ticket, 32), "the ticket is 32 hex digits");
    assert!(is_lower_hex(&card.hash, 64), "the hash is 64 lower-case hex digits");
    assert_eq!(card.effect, VerbEffect::Write);
    assert_eq!(card.verb, "delete_pod");
    assert_eq!(card.program, "kubectl");
    assert_eq!(
        card.argv,
        ["delete", "pod", "--context=prod", "--namespace=payments", "--wait=false", "--", "api-0"]
    );
    let names: Vec<&str> = card.fields.iter().map(|(name, _)| name.as_str()).collect();
    for expected in ["tool", "verb", "effect", "arg 1", "arg 7", "context", "namespace", "guard"] {
        assert!(names.contains(&expected), "the card holds the field {expected}");
    }
    let approved = table.confirm(&toy, &card.ticket, &card.hash).unwrap();
    assert_eq!(approved.tool(), ToolId::Kube);
    assert_eq!(approved.verb().id, "delete_pod");
    assert_eq!(approved.pane(), Some(3));
    let argv: Vec<String> = approved.argv().iter().map(|a| a.to_str().unwrap().to_owned()).collect();
    assert_eq!(argv, card.argv, "what will run is what the card showed");
}

#[test]
fn a_second_confirm_of_the_same_ticket_is_unknown() {
    let toy = Toy::new();
    let table = TicketTable::new();
    let card = table.prepare(&toy, ToolId::Kube, "delete_pod", None, &pod("api-0")).unwrap();
    table.confirm(&toy, &card.ticket, &card.hash).unwrap();
    let again = table.confirm(&toy, &card.ticket, &card.hash).err();
    assert_eq!(again, Some(TicketError::Unknown));
}

#[test]
fn a_read_verb_needs_no_ticket() {
    let toy = Toy::new();
    let table = TicketTable::new();
    let refused = table.prepare(&toy, ToolId::Kube, "pods", None, &BTreeMap::new()).err();
    assert_eq!(refused, Some(TicketError::NotGated));
}

#[test]
fn two_prepares_of_one_request_differ_in_ticket_and_agree_in_hash() {
    let toy = Toy::new();
    let table = TicketTable::new();
    let one = table.prepare(&toy, ToolId::Kube, "delete_pod", None, &pod("api-0")).unwrap();
    let two = table.prepare(&toy, ToolId::Kube, "delete_pod", None, &pod("api-0")).unwrap();
    assert_ne!(one.ticket, two.ticket, "every card has its own id");
    assert_eq!(one.hash, two.hash, "the hash depends on the request only");
}
