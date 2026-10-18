//! The approval pipeline for the verbs that change something: a card, a hash that binds it to the request, a confirm that
//! resolves the request again, and an approval that only that confirm can make.
//!
//! The tests run against a toy resolver that stands in for the service. Hostile names are built from code points, never
//! typed, so that this file holds no hidden character itself.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// The same id with another effect: a verb whose effect changes between prepare and confirm.
static DELETE_POD_AS_EXEC: Verb = Verb {
    id: "delete_pod",
    label: "Delete pod",
    effect: VerbEffect::Exec,
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
    production: Option<String>,
    wait: String,
    effect_is_exec: bool,
    fails: bool,
}

/// A toy that resolves `kube` verbs the way the service will: from the request and from the selection it holds.
struct Toy {
    world: Mutex<World>,
}

impl Toy {
    fn new() -> Self {
        Self {
            world: Mutex::new(World {
                context: "prod".to_owned(),
                namespace: "payments".to_owned(),
                production: None,
                wait: "false".to_owned(),
                effect_is_exec: false,
                fails: false,
            }),
        }
    }

    fn change(&self, change: impl FnOnce(&mut World)) {
        change(&mut self.world.lock().unwrap());
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
        if world.fails {
            return Err(TicketError::Refused);
        }
        match verb {
            "delete_pod" => {
                let pod = args.get("pod").ok_or(TicketError::Refused)?;
                Ok(Resolution {
                    verb: if world.effect_is_exec { &DELETE_POD_AS_EXEC } else { &DELETE_POD },
                    argv: ["delete", "pod"]
                        .iter()
                        .map(OsString::from)
                        .chain([
                            OsString::from(format!("--context={}", world.context)),
                            OsString::from(format!("--namespace={}", world.namespace)),
                            OsString::from(format!("--wait={}", world.wait)),
                            OsString::from("--"),
                            OsString::from(pod),
                        ])
                        .collect(),
                    program: "kubectl".to_owned(),
                    target: vec![
                        ("context".to_owned(), world.context.clone()),
                        ("namespace".to_owned(), world.namespace.clone()),
                    ],
                    production: world.production.clone(),
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

/// A table whose clock the test moves: no test waits.
fn table_with_clock() -> (TicketTable, Arc<Mutex<Duration>>) {
    let base = Instant::now();
    let moved = Arc::new(Mutex::new(Duration::ZERO));
    let seen = Arc::clone(&moved);
    (TicketTable::with_clock(move || base + *seen.lock().unwrap()), moved)
}

fn advance(clock: &Arc<Mutex<Duration>>, by: Duration) {
    *clock.lock().unwrap() += by;
}

fn prepare_pod(table: &TicketTable, toy: &Toy, name: &str) -> holzwave_core::integrate::ticket::Card {
    table.prepare(toy, ToolId::Kube, "delete_pod", None, &pod(name)).unwrap()
}

#[test]
fn the_table_can_be_shared_between_threads() {
    fn shared<T: Send + Sync>() {}
    shared::<TicketTable>();
}

#[test]
fn the_confirm_resolves_again_and_refuses_any_change() {
    let changes: Vec<(&str, Box<dyn Fn(&mut World)>)> = vec![
        ("the context differs", Box::new(|w| w.context = "staging".to_owned())),
        ("the namespace differs", Box::new(|w| w.namespace = "billing".to_owned())),
        ("an argument differs", Box::new(|w| w.wait = "true".to_owned())),
        ("the production name appears", Box::new(|w| w.production = Some("prod".to_owned()))),
        ("the effect differs", Box::new(|w| w.effect_is_exec = true)),
        ("the request no longer resolves", Box::new(|w| w.fails = true)),
    ];
    for (what, change) in &changes {
        let toy = Toy::new();
        let table = TicketTable::new();
        let card = prepare_pod(&table, &toy, "api-0");
        toy.change(|w| change(w));
        let result = table.confirm(&toy, &card.ticket, &card.hash).err();
        assert_eq!(result, Some(TicketError::Changed), "{what}: a changed request was approved");
        assert_eq!(table.pending(), 0, "{what}: the ticket must be gone after a refused confirm");
        let again = table.confirm(&toy, &card.ticket, &card.hash).err();
        assert_eq!(again, Some(TicketError::Unknown), "{what}: the ticket must not come back");
    }
    // The production name disappearing is a change as well.
    let toy = Toy::new();
    toy.change(|w| w.production = Some("prod".to_owned()));
    let table = TicketTable::new();
    let card = prepare_pod(&table, &toy, "api-0");
    assert_eq!(card.guard.production.as_deref(), Some("prod"));
    toy.change(|w| w.production = None);
    assert_eq!(table.confirm(&toy, &card.ticket, &card.hash).err(), Some(TicketError::Changed));
    // And an unchanged world is approved, so the refusals above are not a table that refuses everything.
    let toy = Toy::new();
    let table = TicketTable::new();
    let card = prepare_pod(&table, &toy, "api-0");
    assert!(table.confirm(&toy, &card.ticket, &card.hash).is_ok());
}

/// One resolution that the test edits one field at a time.
struct Fixed(Mutex<Resolution>);

impl Resolve for Fixed {
    fn resolve(&self, _: ToolId, _: &str, _: Option<u64>, _: &BTreeMap<String, String>) -> Result<Resolution, TicketError> {
        Ok(self.0.lock().unwrap().clone())
    }
}

fn base_resolution() -> Resolution {
    Resolution {
        verb: &DELETE_POD,
        argv: ["delete", "pod", "--context=prod", "--", "api-0"].iter().map(OsString::from).collect(),
        program: "kubectl".to_owned(),
        target: vec![("context".to_owned(), "prod".to_owned()), ("namespace".to_owned(), "payments".to_owned())],
        production: Some("prod".to_owned()),
    }
}

#[test]
fn every_field_of_the_card_is_in_the_hash() {
    static OTHER_VERB: Verb = Verb {
        id: "delete_pods",
        label: "Delete pod",
        effect: VerbEffect::Write,
        output: Output::Done,
        args: &POD_ARG,
        refresh_secs: None,
        exit_is_state: false,
    };
    let hash_of = |tool: ToolId, resolution: Resolution| {
        let table = TicketTable::new();
        table.prepare(&Fixed(Mutex::new(resolution)), tool, "delete_pod", None, &BTreeMap::new()).unwrap().hash
    };
    let base = hash_of(ToolId::Kube, base_resolution());
    assert_eq!(base, hash_of(ToolId::Kube, base_resolution()), "the same request hashes the same");

    let mut variants: Vec<(String, String)> = Vec::new();
    let mut add = |what: &str, tool: ToolId, edit: &dyn Fn(&mut Resolution)| {
        let mut resolution = base_resolution();
        edit(&mut resolution);
        variants.push((what.to_owned(), hash_of(tool, resolution)));
    };
    add("the tool", ToolId::Talos, &|_| {});
    add("the verb", ToolId::Kube, &|r| r.verb = &OTHER_VERB);
    add("the effect", ToolId::Kube, &|r| r.verb = &DELETE_POD_AS_EXEC);
    add("the program", ToolId::Kube, &|r| r.program = "kubectl2".to_owned());
    for index in 0..5 {
        add(&format!("argument {index}"), ToolId::Kube, &|r| r.argv[index] = OsString::from("changed"));
    }
    add("an argument added", ToolId::Kube, &|r| r.argv.push(OsString::from("extra")));
    add("an argument removed", ToolId::Kube, &|r| {
        r.argv.pop();
    });
    add("two arguments joined", ToolId::Kube, &|r| {
        let joined = format!("{} {}", r.argv[3].to_str().unwrap(), r.argv[4].to_str().unwrap());
        r.argv.truncate(3);
        r.argv.push(OsString::from(joined));
    });
    add("the first target value", ToolId::Kube, &|r| r.target[0].1 = "dev".to_owned());
    add("the second target value", ToolId::Kube, &|r| r.target[1].1 = "billing".to_owned());
    add("a target label", ToolId::Kube, &|r| r.target[0].0 = "node".to_owned());
    add("a target added", ToolId::Kube, &|r| r.target.push(("node".to_owned(), "n1".to_owned())));
    add("the targets swapped", ToolId::Kube, &|r| r.target.swap(0, 1));
    add("the guard name", ToolId::Kube, &|r| r.production = Some("prod-eu".to_owned()));
    add("the guard removed", ToolId::Kube, &|r| r.production = None);
    let mut seen = BTreeMap::new();
    for (what, hash) in &variants {
        assert_ne!(hash, &base, "{what} is not in the hash");
        if let Some(other) = seen.insert(hash.clone(), what.clone()) {
            panic!("{what} and {other} give the same hash");
        }
    }
}

#[test]
fn three_wrong_hashes_drop_the_ticket() {
    let toy = Toy::new();
    let table = TicketTable::new();
    let card = prepare_pod(&table, &toy, "api-0");
    let wrong = "0".repeat(64);
    assert_eq!(table.confirm(&toy, &card.ticket, &wrong).err(), Some(TicketError::Mismatch));
    assert_eq!(table.pending(), 1, "a wrong hash leaves the card waiting");
    assert_eq!(table.confirm(&toy, &card.ticket, &wrong).err(), Some(TicketError::Mismatch));
    assert_eq!(table.pending(), 1);
    assert_eq!(table.confirm(&toy, &card.ticket, &wrong).err(), Some(TicketError::Mismatch));
    assert_eq!(table.pending(), 0, "the third wrong hash drops the ticket");
    assert_eq!(table.confirm(&toy, &card.ticket, &card.hash).err(), Some(TicketError::Unknown));

    // A correct hash after two wrong ones still works.
    let card = prepare_pod(&table, &toy, "api-0");
    table.confirm(&toy, &card.ticket, &wrong).err();
    table.confirm(&toy, &card.ticket, &wrong).err();
    assert!(table.confirm(&toy, &card.ticket, &card.hash).is_ok());
}

#[test]
fn a_ticket_expires() {
    let toy = Toy::new();
    let (table, clock) = table_with_clock();
    let late = prepare_pod(&table, &toy, "api-0");
    let last = prepare_pod(&table, &toy, "api-1");
    advance(&clock, Duration::from_secs(119));
    assert!(table.confirm(&toy, &last.ticket, &last.hash).is_ok(), "119 seconds is still in time");
    advance(&clock, Duration::from_secs(1));
    assert_eq!(table.confirm(&toy, &late.ticket, &late.hash).err(), Some(TicketError::Expired));
    assert_eq!(table.confirm(&toy, &late.ticket, &late.hash).err(), Some(TicketError::Unknown), "an expired ticket is gone");

    // A card nobody answered does not wait for ever and is not counted as pending.
    let card = prepare_pod(&table, &toy, "api-2");
    assert_eq!(table.pending(), 1);
    advance(&clock, Duration::from_secs(120));
    assert_eq!(table.pending(), 0);
    assert_eq!(table.confirm(&toy, &card.ticket, &card.hash).err(), Some(TicketError::Unknown));
    assert_eq!(card.expires_in_secs, 120);
}

#[test]
fn at_most_eight_tickets_wait() {
    let toy = Toy::new();
    let table = TicketTable::new();
    let cards: Vec<_> = (0..9).map(|i| prepare_pod(&table, &toy, &format!("api-{i}"))).collect();
    assert_eq!(table.pending(), 8);
    assert_eq!(table.confirm(&toy, &cards[0].ticket, &cards[0].hash).err(), Some(TicketError::Unknown), "the oldest was dropped");
    for card in &cards[1..] {
        assert!(table.confirm(&toy, &card.ticket, &card.hash).is_ok(), "the other eight still work");
    }
    assert_eq!(table.pending(), 0);

    let card = prepare_pod(&table, &toy, "api-0");
    assert!(table.cancel(&card.ticket));
    assert!(!table.cancel(&card.ticket), "a second cancel finds nothing");
    assert_eq!(table.confirm(&toy, &card.ticket, &card.hash).err(), Some(TicketError::Unknown));

    // Expired tickets do not take a place away from a new one.
    let (table, clock) = table_with_clock();
    let first = prepare_pod(&table, &toy, "api-0");
    advance(&clock, Duration::from_secs(120));
    let fresh: Vec<_> = (0..8).map(|i| prepare_pod(&table, &toy, &format!("api-{i}"))).collect();
    assert_eq!(table.confirm(&toy, &first.ticket, &first.hash).err(), Some(TicketError::Unknown));
    for card in &fresh {
        assert!(table.confirm(&toy, &card.ticket, &card.hash).is_ok());
    }
}

/// Hashes that are not the card's and are not even well formed, made from the card's own hash.
fn malformed_forms(hash: &str) -> Vec<(&'static str, String)> {
    let mut not_hex = hash.to_owned();
    not_hex.replace_range(0..1, "g");
    vec![
        ("upper case", hash.to_ascii_uppercase()),
        ("a leading space", format!(" {hash}")),
        ("a trailing space", format!("{hash} ")),
        ("63 characters", hash[..63].to_owned()),
        ("65 characters", format!("{hash}0")),
        ("a character that is not hex", not_hex),
        ("empty", String::new()),
    ]
}

#[test]
fn a_malformed_hash_is_a_wrong_hash() {
    let toy = Toy::new();
    let table = TicketTable::new();
    for index in 0..7 {
        let card = prepare_pod(&table, &toy, "api-0");
        let (what, bad) = malformed_forms(&card.hash).swap_remove(index);
        if bad == card.hash {
            continue; // a hash with no letter in it has no upper-case form
        }
        let result = table.confirm(&toy, &card.ticket, &bad).err();
        assert_eq!(result, Some(TicketError::Mismatch), "{what} must be a wrong hash");
        assert!(table.cancel(&card.ticket), "{what}: the ticket still waits after one wrong answer");
    }
    // Each one counts as an attempt: three of them drop the ticket.
    let card = prepare_pod(&table, &toy, "api-0");
    for (_, bad) in malformed_forms(&card.hash).into_iter().skip(3).take(3) {
        let _ = table.confirm(&toy, &card.ticket, &bad);
    }
    assert_eq!(table.pending(), 0, "three malformed answers drop the ticket");
    assert_eq!(table.confirm(&toy, &card.ticket, &card.hash).err(), Some(TicketError::Unknown));
}

#[test]
fn two_confirms_at_once_approve_one() {
    let toy = Toy::new();
    let table = TicketTable::new();
    for round in 0..50 {
        let card = prepare_pod(&table, &toy, &format!("api-{round}"));
        let approvals: usize = std::thread::scope(|scope| {
            let handles: Vec<_> =
                (0..4).map(|_| scope.spawn(|| usize::from(table.confirm(&toy, &card.ticket, &card.hash).is_ok()))).collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(approvals, 1, "round {round}: a ticket is approved once");
    }
}
