//! The approval pipeline for the verbs that change something: a card, a hash that binds it to the request, a confirm
//! that resolves the request again, and an approval that only that confirm can make.
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
    fn resolve(
        &self,
        _: ToolId,
        _: &str,
        _: Option<u64>,
        _: &BTreeMap<String, String>,
    ) -> Result<Resolution, TicketError> {
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
    let gone = table.confirm(&toy, &late.ticket, &late.hash).err();
    assert_eq!(gone, Some(TicketError::Unknown), "an expired ticket is gone");

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
    let oldest = table.confirm(&toy, &cards[0].ticket, &cards[0].hash).err();
    assert_eq!(oldest, Some(TicketError::Unknown), "the oldest was dropped");
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
            let confirm = || usize::from(table.confirm(&toy, &card.ticket, &card.hash).is_ok());
            let handles: Vec<_> = (0..4).map(|_| scope.spawn(confirm)).collect();
            handles.into_iter().map(|h| h.join().unwrap()).sum()
        });
        assert_eq!(approvals, 1, "round {round}: a ticket is approved once");
    }
}

/// A scalar value from its code point: the hostile characters are never typed in this file.
fn ch(code: u32) -> char {
    char::from_u32(code).expect("a scalar value")
}

/// Whether a character would hide something on a card: controls, direction marks, zero-width, format and tag
/// characters.
fn hides(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

fn assert_nothing_hidden(card: &holzwave_core::integrate::ticket::Card, what: &str) {
    let mut texts: Vec<&str> = vec![&card.program, &card.hash, &card.ticket];
    texts.extend(card.argv.iter().map(String::as_str));
    for (label, value) in card.target.iter().chain(card.fields.iter()) {
        texts.push(label);
        texts.push(value);
    }
    if let Some(name) = &card.guard.production {
        texts.push(name);
    }
    for text in texts {
        assert!(!text.chars().any(hides), "{what}: the card holds a raw hidden character");
    }
}

fn prepare_fixed(resolution: Resolution) -> Result<holzwave_core::integrate::ticket::Card, TicketError> {
    TicketTable::new().prepare(&Fixed(Mutex::new(resolution)), ToolId::Kube, "delete_pod", None, &BTreeMap::new())
}

#[test]
fn a_hostile_name_is_spelled_out_in_the_card() {
    let cases: [(&str, String, &str); 6] = [
        ("a line break", format!("a{}b", ch(0x0A)), "a\\nb"),
        ("a right-to-left override", format!("a{}b", ch(0x202E)), "a\\u{202e}b"),
        ("a terminal escape sequence", format!("a{}[2Jb", ch(0x1B)), "a\\u{1b}[2Jb"),
        ("a zero-width space", format!("a{}b", ch(0x200B)), "a\\u{200b}b"),
        ("a tag character", format!("a{}b", ch(0xE0041)), "a\\u{e0041}b"),
        ("a carriage return and a tab", format!("a{}{}b", ch(0x0D), ch(0x09)), "a\\r\\tb"),
    ];
    let mut hashes = Vec::new();
    for (what, name, spelled) in &cases {
        let mut resolution = base_resolution();
        resolution.argv[4] = OsString::from(name);
        resolution.target[0].1 = name.clone();
        resolution.target[1].0 = name.clone();
        resolution.program = name.clone();
        resolution.production = Some(name.clone());
        let card = prepare_fixed(resolution).unwrap();
        assert_eq!(card.argv[4], *spelled, "{what}: the argument is spelled out");
        assert_eq!(card.target[0].1, *spelled, "{what}: the target value is spelled out");
        assert_eq!(card.target[1].0, *spelled, "{what}: the target label is spelled out");
        assert_eq!(card.program, *spelled, "{what}: the program is spelled out");
        assert_eq!(card.guard.production.as_deref(), Some(*spelled), "{what}: the production name is spelled out");
        assert_nothing_hidden(&card, what);
        hashes.push(card.hash);
    }
    hashes.sort();
    hashes.dedup();
    assert_eq!(hashes.len(), cases.len(), "different hostile names give different hashes");

    // Typed escape text and the character it stands for are two different names and two different hashes.
    let mut typed = base_resolution();
    typed.argv[4] = OsString::from("a\\nb");
    let mut real = base_resolution();
    real.argv[4] = OsString::from(format!("a{}b", ch(0x0A)));
    let typed = prepare_fixed(typed).unwrap();
    let real = prepare_fixed(real).unwrap();
    assert_ne!(typed.argv[4], real.argv[4]);
    assert_ne!(typed.hash, real.hash);
}

#[cfg(unix)]
#[test]
fn an_argument_that_is_not_text_is_refused() {
    use std::os::unix::ffi::OsStringExt;
    let mut resolution = base_resolution();
    resolution.argv[4] = OsString::from_vec(vec![b'a', 0xFF, b'b']);
    assert_eq!(prepare_fixed(resolution).err(), Some(TicketError::Refused));
}

#[test]
fn the_guard_is_part_of_the_hashed_fields() {
    let field = |card: &holzwave_core::integrate::ticket::Card, name: &str| -> Option<String> {
        card.fields.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    };
    let production = prepare_fixed(base_resolution()).unwrap();
    assert_eq!(production.guard.production.as_deref(), Some("prod"));
    assert_eq!(field(&production, "guard").as_deref(), Some("production"));
    assert_eq!(field(&production, "production_name").as_deref(), Some("prod"));

    let mut plain = base_resolution();
    plain.production = None;
    let plain = prepare_fixed(plain).unwrap();
    assert_eq!(plain.guard.production, None);
    assert_eq!(field(&plain, "guard").as_deref(), Some("none"));
    assert_eq!(field(&plain, "production_name"), None, "no name when the context is not production");
    assert_ne!(production.hash, plain.hash);

    // A production guard that names nothing is refused: the person could not tell which one it is.
    let mut nameless = base_resolution();
    nameless.production = Some(String::new());
    assert_eq!(prepare_fixed(nameless).err(), Some(TicketError::Refused));

    // The fields keep their documented order.
    let names: Vec<&str> = production.fields.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "tool", "verb", "effect", "program", "arg 1", "arg 2", "arg 3", "arg 4", "arg 5", "context", "namespace",
            "guard", "production_name"
        ]
    );
}

#[test]
fn a_local_verb_names_the_file_it_changes() {
    static USE_CONTEXT: Verb = Verb {
        id: "use_context",
        label: "Make default",
        effect: VerbEffect::Local,
        output: Output::Done,
        args: &[],
        refresh_secs: None,
        exit_is_state: false,
    };
    let local = |target: Vec<(String, String)>| Resolution {
        verb: &USE_CONTEXT,
        argv: ["config", "use-context", "--", "dev"].iter().map(OsString::from).collect(),
        program: "kubectl".to_owned(),
        target,
        production: None,
    };
    let sentence = "the kubeconfig files named by KUBECONFIG, by default ~/.kube/config";
    let card = prepare_fixed(local(vec![("changes".to_owned(), sentence.to_owned())])).unwrap();
    assert_eq!(card.effect, VerbEffect::Local);
    assert!(card.target.contains(&("changes".to_owned(), sentence.to_owned())));
    assert!(card.fields.contains(&("changes".to_owned(), sentence.to_owned())), "the sentence is hashed");
    // A local change that does not say what it changes is refused: the card would hide the one fact that matters.
    assert_eq!(prepare_fixed(local(vec![("context".to_owned(), "dev".to_owned())])).err(), Some(TicketError::Refused));
    assert_eq!(prepare_fixed(local(Vec::new())).err(), Some(TicketError::Refused));
}

#[test]
fn a_card_is_bounded() {
    let total = |card: &holzwave_core::integrate::ticket::Card| -> usize {
        card.fields.iter().map(|(n, v)| n.len() + v.len()).sum()
    };
    let with_arg = |text: &str| {
        let mut resolution = base_resolution();
        resolution.argv[4] = OsString::from(text);
        prepare_fixed(resolution)
    };
    let empty = with_arg("").unwrap();
    let room = 8192 - total(&empty);
    let full = with_arg(&"a".repeat(room)).unwrap();
    assert_eq!(total(&full), 8192, "a card of exactly 8 KiB of field text is allowed");
    assert_eq!(with_arg(&"a".repeat(room + 1)).err(), Some(TicketError::Refused), "one byte more is refused");
    // A hostile 5000-character argument is cut by the validators long before; two of them are over the card's limit.
    let mut two = base_resolution();
    two.argv[3] = OsString::from("a".repeat(5000));
    two.argv[4] = OsString::from("b".repeat(5000));
    assert_eq!(prepare_fixed(two).err(), Some(TicketError::Refused));
    // The limit counts the card as shown: a character that is spelled out grows.
    let lines = ch(0x0A).to_string().repeat(room);
    assert_eq!(with_arg(&lines).err(), Some(TicketError::Refused), "the spelled text is what counts");
    // A target and a label count too.
    let mut target = base_resolution();
    target.target[0].1 = "t".repeat(9000);
    assert_eq!(prepare_fixed(target).err(), Some(TicketError::Refused));

    // The stored request is bounded as well: it is kept for 120 seconds, up to eight times.
    let toy = Toy::new();
    let table = TicketTable::new();
    let try_request =
        |verb: &str, args: &BTreeMap<String, String>| table.prepare(&toy, ToolId::Kube, verb, None, args).err();
    assert_eq!(try_request("delete_pod", &pod(&"a".repeat(4097))), Some(TicketError::Refused));
    assert_eq!(try_request(&"v".repeat(65), &pod("api-0")), Some(TicketError::Refused));
    let many: BTreeMap<String, String> = (0..17).map(|i| (format!("k{i}"), "v".to_owned())).collect();
    assert_eq!(try_request("delete_pod", &many), Some(TicketError::Refused));
    let long_key = BTreeMap::from([("k".repeat(65), "v".to_owned())]);
    assert_eq!(try_request("delete_pod", &long_key), Some(TicketError::Refused));
    assert_eq!(table.pending(), 0, "a refused request leaves no ticket");
}

mod source {
    use std::fs;
    use std::path::{Path, PathBuf};

    pub fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("core/ has a parent: the workspace root").to_path_buf()
    }

    /// Every file below `dir`, read as text at run time.
    pub fn files(dir: &Path, extension: Option<&str>, out: &mut Vec<(String, String)>) {
        let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("{} must exist: {e}", dir.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files(&path, extension, out);
            } else if extension.is_none_or(|e| path.extension().is_some_and(|x| x == e)) {
                let text = fs::read(&path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                let relative = path.strip_prefix(root()).unwrap_or(&path).to_string_lossy().replace('\\', "/");
                out.push((relative, text));
            }
        }
    }
}

#[test]
fn only_the_window_can_approve_and_nothing_of_it_is_on_the_socket() {
    let root = source::root();
    let ticket_path = "core/src/integrate/ticket.rs";

    // 1. `Approved` is built in `ticket.rs` only. The AI chat's gate has a private type of the same name: it is allowed
    //    to build its own, and the test checks that it is private.
    let mut sources = Vec::new();
    source::files(&root.join("core/src"), Some("rs"), &mut sources);
    source::files(&root.join("app/src-tauri/src"), Some("rs"), &mut sources);
    assert!(sources.iter().any(|(path, _)| path == ticket_path), "the scan reads the ticket module");
    assert!(sources.iter().any(|(path, _)| path.starts_with("app/src-tauri/src/")), "the scan reads the shell");
    let mut literal_sites = Vec::new();
    for (path, text) in &sources {
        for (number, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            let literal = (code.contains("Approved {") || code.contains("Approved{"))
                && !code.contains("struct Approved")
                && !code.contains("impl");
            if literal {
                literal_sites.push(format!("{path}:{}", number + 1));
            }
        }
    }
    let foreign: Vec<&String> = literal_sites
        .iter()
        .filter(|site| !site.starts_with(ticket_path) && !site.starts_with("core/src/ai/gate.rs:"))
        .collect();
    assert!(foreign.is_empty(), "Approved is built outside ticket.rs: {foreign:?}");
    let gate = sources.iter().find(|(path, _)| path == "core/src/ai/gate.rs").map(|(_, t)| t.as_str()).unwrap_or("");
    assert!(
        !gate.contains("pub struct Approved {") && !gate.contains("pub(crate) struct Approved {"),
        "the gate's own Approved must stay private"
    );
    let own: Vec<&String> = literal_sites.iter().filter(|site| site.starts_with(ticket_path)).collect();
    assert_eq!(own.len(), 1, "exactly one struct literal in ticket.rs: {own:?}");

    // 2. Inside `ticket.rs`: private fields, no derive, one constructor, and `confirm` is the only visible way out.
    let ticket = &sources.iter().find(|(path, _)| path == ticket_path).expect("ticket.rs").1;
    let start = ticket.find("pub struct Approved {").expect("the Approved type");
    let block = &ticket[start..start + ticket[start..].find("\n}").expect("the end of the type")];
    for line in block.lines().skip(1) {
        assert!(!line.trim_start().starts_with("pub"), "a field of Approved is public: {line}");
    }
    let before: Vec<&str> = ticket[..start]
        .lines()
        .rev()
        .take_while(|l| l.trim_start().starts_with("#[") || l.trim_start().starts_with("///"))
        .collect();
    for attribute in before.iter().filter(|l| l.trim_start().starts_with("#[")) {
        for banned in ["Clone", "Copy", "Default", "Serialize", "Deserialize"] {
            assert!(!attribute.contains(banned), "Approved derives {banned}");
        }
    }
    for line in ticket.lines().filter(|l| l.contains("for Approved")) {
        assert!(line.contains("fmt::Debug for Approved"), "an unexpected impl for Approved: {line}");
    }
    let mut visible_returns = Vec::new();
    let lines: Vec<&str> = ticket.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let visible_fn = ["pub fn ", "pub(crate) fn ", "pub(super) fn "].iter().any(|p| trimmed.starts_with(p));
        if !visible_fn {
            continue;
        }
        let mut signature = String::new();
        for next in &lines[index..] {
            signature.push_str(next);
            signature.push(' ');
            if next.contains('{') || next.trim_end().ends_with(';') {
                break;
            }
        }
        let returns = signature.split("->").nth(1).unwrap_or("");
        if returns.contains("Approved") {
            let name = trimmed.split("fn ").nth(1).and_then(|rest| rest.split(['(', '<']).next()).unwrap_or("");
            visible_returns.push(name.to_owned());
        }
    }
    assert_eq!(visible_returns, ["confirm"], "only confirm returns an Approved");
    let code: String = ticket.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    assert_eq!(code.matches("Approved::issue(").count(), 1, "one call of the private constructor");
    let confirm = code.find("pub fn confirm").expect("confirm");
    assert!(code[confirm..].contains("Approved::issue("), "the one call is inside confirm");

    // 3. The socket side knows no ticket, no confirm and no approval type.
    let mut socket = Vec::new();
    source::files(&root.join("ipc/src"), None, &mut socket);
    source::files(&root.join("wsh/src"), None, &mut socket);
    assert!(socket.iter().any(|(path, _)| path == "ipc/src/lib.rs"), "the scan reads the ipc crate");
    assert!(socket.iter().any(|(path, _)| path == "wsh/src/main.rs"), "the scan reads the wsh crate");
    for (path, text) in &socket {
        for word in ["ticket", "Approved", "integration_confirm", "integration_prepare"] {
            assert!(!text.contains(word), "{path} names `{word}`: a pane process could reach the approval");
        }
    }
}
