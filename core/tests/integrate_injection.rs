//! The release gate for names: a hostile name is run through every verb of every tool, in every argument slot and in
//! every selection field, and each request ends one of two ways. It is refused before any process exists (the fake that
//! records its own start never started), or the argument list the fake recorded is exactly the list a harmless name
//! gives, with the name in the place of the harmless one: one element, either `--flag=<name>` or the name after `--`.
//!
//! The slots are not written down here: they are generated from the compiled-in descriptions of the four tools, so a
//! verb or a selection field that is added later is covered at once. For every slot a request with a harmless value is
//! run first and its list is the baseline; a slot whose baseline does not show the harmless value is not a slot of that
//! verb (the verb ignores the field) and is counted as skipped. The counts per class are printed and a minimum of each
//! is asserted, so the corpus cannot shrink without a test failing.
//!
//! Nothing here uses a real tool or a network. The tools are fake programs of the rig on a scratch computer.
#![cfg(unix)]

mod common;
#[path = "common/fake_cli.rs"]
mod fake_cli;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use holzwave_core::integrate::kube::POD_SHELL;
use holzwave_core::integrate::service::{Done, Fault, FaultCode, HostFacts, IntegrationService};
use holzwave_core::integrate::{spec_of, ArgKind, ArgSpec, Output, SelectKind, Selection, ToolId, Verb, VerbEffect};
use holzwave_core::secrets::MemoryStore;

use common::scratch_dir;
use fake_cli::FakeCli;

// ---- the scratch computer --------------------------------------------------------------------------------------------

/// A scratch computer with a fake for each of the four tools, and the service over it.
struct Machine {
    root: PathBuf,
    fakes: Vec<(ToolId, FakeCli)>,
    service: IntegrationService,
}

impl Machine {
    fn new() -> Machine {
        let root = scratch_dir("injection");
        let (bin, home, data) = (root.join("bin"), root.join("home"), root.join("data"));
        for dir in [&bin, &home, &data] {
            std::fs::create_dir(dir).expect("a directory of the rig");
        }
        let programs = [(ToolId::Kube, "kubectl"), (ToolId::Talos, "talosctl"), (ToolId::Azure, "az"), (ToolId::Gam, "gam")];
        let fakes = programs.iter().map(|(tool, name)| (*tool, FakeCli::new(&bin, name).prints("{}\n"))).collect();
        let facts = HostFacts::for_tests(&root, std::slice::from_ref(&bin), &home, &data);
        let service = IntegrationService::new(&data, Arc::new(MemoryStore::default()), facts, &[]);
        Machine { root, fakes, service }
    }

    fn argv_file(fake: &FakeCli) -> String {
        format!("{}.argv", fake.path().display())
    }

    /// Forgets every start so far, so that the next one is the only one on record.
    fn clear(&self) {
        for (_, fake) in &self.fakes {
            let _ = std::fs::remove_file(Self::argv_file(fake));
        }
    }

    fn fake(&self, tool: ToolId) -> &FakeCli {
        &self.fakes.iter().find(|(own, _)| *own == tool).expect("a fake for every tool").1
    }

    fn started(&self, tool: ToolId) -> bool {
        self.fake(tool).started()
    }

    fn any_started(&self) -> bool {
        self.fakes.iter().any(|(_, fake)| fake.started())
    }

    /// The list the last start of the tool's fake was given.
    fn recorded(&self, tool: ToolId) -> Vec<String> {
        self.fake(tool).argv().iter().map(|arg| String::from_utf8_lossy(arg).into_owned()).collect()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

// ---- kinds, slots and the harmless values ------------------------------------------------------------------------

/// What a slot holds, as far as the validators are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Context,
    Namespace,
    Pod,
    Container,
    Node,
    Subscription,
    Section,
    Lines,
    Flag,
    Choice,
    Profile,
}

const EVERY_KIND: &[Kind] = &[
    Kind::Context,
    Kind::Namespace,
    Kind::Pod,
    Kind::Container,
    Kind::Node,
    Kind::Subscription,
    Kind::Section,
    Kind::Lines,
    Kind::Flag,
    Kind::Choice,
    Kind::Profile,
];

fn kind_of_arg(kind: ArgKind) -> Kind {
    match kind {
        ArgKind::Context => Kind::Context,
        ArgKind::Namespace => Kind::Namespace,
        ArgKind::Pod => Kind::Pod,
        ArgKind::Container => Kind::Container,
        ArgKind::Node => Kind::Node,
        ArgKind::Subscription => Kind::Subscription,
        ArgKind::Section => Kind::Section,
        ArgKind::Lines => Kind::Lines,
        ArgKind::Flag => Kind::Flag,
        ArgKind::Choice(_) => Kind::Choice,
    }
}

fn kind_of_select(kind: SelectKind) -> Kind {
    match kind {
        SelectKind::Context => Kind::Context,
        SelectKind::Namespace => Kind::Namespace,
        SelectKind::Subscription => Kind::Subscription,
        SelectKind::Section => Kind::Section,
        SelectKind::Node => Kind::Node,
    }
}

/// The harmless value of an argument. The value of a selection field differs from it, so that a list shows which of the
/// two it came from.
fn harmless_arg(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::Context => "ctx-arg",
        ArgKind::Namespace => "ns-arg",
        ArgKind::Pod => "pod-arg",
        ArgKind::Container => "ctr-arg",
        ArgKind::Node => "10.9.8.7",
        ArgKind::Subscription => "11111111-2222-4333-8444-555555555555",
        ArgKind::Section => "sec-arg",
        ArgKind::Lines => "123",
        ArgKind::Flag => "true",
        ArgKind::Choice(words) => words[0],
    }
}

fn harmless_select(kind: SelectKind) -> &'static str {
    match kind {
        SelectKind::Context => "ctx-sel",
        SelectKind::Namespace => "ns-sel",
        SelectKind::Subscription => "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee",
        SelectKind::Section => "sec-sel",
        SelectKind::Node => "10.9.8.8",
    }
}

/// The text a kind keeps of an accepted value: a subscription is kept in lower case.
fn canonical(kind: Kind, value: &str) -> String {
    if kind == Kind::Subscription { value.to_ascii_lowercase() } else { value.to_owned() }
}

#[derive(Clone, Copy)]
enum What {
    Arg(&'static ArgSpec),
    Select(SelectKind),
    Profile,
}

#[derive(Clone, Copy)]
struct Slot {
    tool: ToolId,
    verb: &'static Verb,
    what: What,
    /// Every value of the corpus is run in a full slot; a selection field against a second or later verb runs a sample
    /// that still holds values of every class (that verb only builds its list differently).
    full: bool,
}

impl Slot {
    fn kind(&self) -> Kind {
        match self.what {
            What::Arg(spec) => kind_of_arg(spec.kind),
            What::Select(kind) => kind_of_select(kind),
            What::Profile => Kind::Profile,
        }
    }

    fn label(&self) -> String {
        let place = match self.what {
            What::Arg(spec) => format!("argument {}", spec.name),
            What::Select(kind) => format!("selection {kind:?}"),
            What::Profile => "selection Profile".to_owned(),
        };
        format!("{}/{}/{place}", self.tool.as_str(), self.verb.id)
    }

    fn token(&self) -> &'static str {
        match self.what {
            What::Arg(spec) => harmless_arg(spec.kind),
            What::Select(kind) => harmless_select(kind),
            What::Profile => "",
        }
    }
}

/// Every slot the descriptions have: each declared argument of each verb, each selection field against each verb, and
/// the profile field against the first verb of the tool.
fn slots_of(tool: ToolId) -> Vec<Slot> {
    let spec = spec_of(tool);
    let mut slots = Vec::new();
    for verb in spec.verbs {
        for arg in verb.args {
            slots.push(Slot { tool, verb, what: What::Arg(arg), full: true });
        }
        for kind in tool.selects() {
            slots.push(Slot { tool, verb, what: What::Select(*kind), full: false });
        }
    }
    slots.push(Slot { tool, verb: &spec.verbs[0], what: What::Profile, full: true });
    slots
}

// ---- one request ---------------------------------------------------------------------------------------------------

fn next_pane() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(10_000);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn selection_with(field: SelectKind, value: &str) -> Selection {
    let mut selection = Selection::default();
    let value = Some(value.to_owned());
    match field {
        SelectKind::Context => selection.context = value,
        SelectKind::Namespace => selection.namespace = value,
        SelectKind::Subscription => selection.subscription = value,
        SelectKind::Section => selection.section = value,
        SelectKind::Node => selection.node = value,
    }
    selection
}

/// The call that starts, or describes, `verb`, with the pane the selection lives in. `Ok` is the list that the tool is
/// given, as recorded by the fake or, for a card and a launch that start nothing, as the card or the launch has it;
/// `Err` is a refusal, and then no process exists. Anything else is a failure of the test.
fn call(
    m: &Machine,
    tool: ToolId,
    verb: &'static Verb,
    args: &BTreeMap<String, String>,
    pane: Option<u64>,
) -> Result<Vec<String>, Fault> {
    m.clear();
    let name = tool.as_str();
    let lane_view = verb.effect == VerbEffect::Read && verb.output == Output::View;
    let lane_terminal = verb.effect == VerbEffect::Read && verb.output == Output::Terminal;
    if lane_view {
        return match m.service.run_view(name, verb.id, pane, args) {
            Err(fault) if fault.code == FaultCode::Refused => {
                assert!(!m.any_started(), "a refused read started a process: {} {:?}", verb.id, args);
                Err(fault)
            }
            other => {
                assert!(m.started(tool), "a read that was not refused did not start the tool: {other:?}");
                Ok(m.recorded(tool))
            }
        };
    }
    if lane_terminal {
        return match m.service.launch(name, verb.id, pane, args) {
            Err(fault) if fault.code == FaultCode::Refused => {
                assert!(!m.any_started(), "a refused launch started a process: {} {:?}", verb.id, args);
                Err(fault)
            }
            Ok(launch) => {
                // The shell of a pane reads the list of contexts to write its private overlay; every other launch only
                // describes a pane and starts nothing.
                if verb.id != "shell" {
                    assert!(!m.any_started(), "a launch started a process before the pane did: {}", verb.id);
                }
                Ok(launch.args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect())
            }
            Err(fault) => panic!("a launch gave {fault:?} for {} {args:?}", verb.id),
        };
    }
    match m.service.prepare(name, verb.id, pane, args) {
        Err(fault) if fault.code == FaultCode::Refused => {
            assert!(!m.any_started(), "a refused card started a process: {} {:?}", verb.id, args);
            Err(fault)
        }
        Err(fault) => panic!("a card gave {fault:?} for {} {args:?}", verb.id),
        Ok(card) => {
            assert!(!m.any_started(), "a card started a process before it was confirmed: {}", verb.id);
            // The card spells a list as the person reads it: a backslash is doubled. A name that is accepted holds no
            // other character the card would spell out, so the card and the list differ in that one way only.
            let shown = |actual: &[String]| -> Vec<String> { actual.iter().map(|arg| arg.replace('\\', "\\\\")).collect() };
            let actual = match m.service.confirm(&card.ticket, &card.hash) {
                Ok(Done::Ran { .. }) => {
                    assert!(m.started(tool), "a confirmed {} did not run", verb.id);
                    m.recorded(tool)
                }
                Ok(Done::Launch(launch)) => {
                    assert!(!m.any_started(), "a confirmed session started a process before the pane did");
                    launch.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
                }
                Err(fault) => panic!("a confirm gave {fault:?} for {} {:?}", verb.id, card.argv),
            };
            assert_eq!(shown(&actual), card.argv, "the list the tool got is not the list of the card");
            Ok(actual)
        }
    }
}

/// `value` in the slot, everything else harmless.
fn exercise(m: &Machine, slot: Slot, value: &str) -> Result<Vec<String>, Fault> {
    let mut args = BTreeMap::new();
    match slot.what {
        What::Arg(spec) => {
            for own in slot.verb.args {
                args.insert(own.name.to_owned(), harmless_arg(own.kind).to_owned());
            }
            args.insert(spec.name.to_owned(), value.to_owned());
            // A pane of its own, with no selection: no request is answered from the cache of an earlier one.
            call(m, slot.tool, slot.verb, &args, Some(next_pane()))
        }
        What::Select(_) | What::Profile => {
            // Only what the verb needs: an optional argument of the same kind would win over the selection.
            for own in slot.verb.args.iter().filter(|own| own.required) {
                args.insert(own.name.to_owned(), harmless_arg(own.kind).to_owned());
            }
            let selection = match slot.what {
                What::Select(field) => selection_with(field, value),
                _ => Selection { profile: Some(value.to_owned()), ..Selection::default() },
            };
            let pane = next_pane();
            m.clear();
            let result = match m.service.select(slot.tool.as_str(), Some(pane), &selection) {
                Err(fault) => {
                    assert_eq!(fault.code, FaultCode::Refused, "a selection gave {fault:?}");
                    assert!(!m.any_started(), "a refused selection started a process");
                    Err(fault)
                }
                Ok(_) => call(m, slot.tool, slot.verb, &args, Some(pane)),
            };
            m.service.forget_pane(pane);
            result
        }
    }
}

// ---- the corpus ----------------------------------------------------------------------------------------------------

/// A hostile name and what is known of it before it is run.
struct Hostile {
    class: &'static str,
    value: String,
    /// The kinds that must take it.
    accept: &'static [Kind],
    /// The kinds that must refuse it.
    refuse: &'static [Kind],
}

fn refused_everywhere(class: &'static str, value: impl Into<String>) -> Hostile {
    Hostile { class, value: value.into(), accept: &[], refuse: EVERY_KIND }
}

/// A value that only a context may be: a context is any printable text, the other kinds have a narrow charset.
fn context_only(class: &'static str, value: impl Into<String>) -> Hostile {
    Hostile {
        class,
        value: value.into(),
        accept: &[Kind::Context],
        refuse: &[
            Kind::Namespace,
            Kind::Pod,
            Kind::Container,
            Kind::Node,
            Kind::Subscription,
            Kind::Section,
            Kind::Lines,
            Kind::Flag,
        ],
    }
}

fn custom(
    class: &'static str,
    value: impl Into<String>,
    accept: &'static [Kind],
    refuse: &'static [Kind],
) -> Hostile {
    Hostile { class, value: value.into(), accept, refuse }
}

/// Only the invariant is asserted.
fn any_outcome(class: &'static str, value: impl Into<String>) -> Hostile {
    Hostile { class, value: value.into(), accept: &[], refuse: &[] }
}

fn ch(code: u32) -> String {
    char::from_u32(code).expect("a scalar value").to_string()
}

/// The hostile names, generated: every class has more than one value, the invisible ones are built from code points so
/// that this file holds none of them.
fn hostile_names() -> Vec<Hostile> {
    use Kind::{Container, Context, Flag, Lines, Namespace, Node, Pod, Section, Subscription};
    // The kinds that take a lower-case word of letters and digits.
    const WORDS: &[Kind] = &[Context, Namespace, Pod, Container, Node, Section];
    const NOT_WORDS: &[Kind] = &[Subscription, Lines, Flag];
    // The kinds that take a number of digits that is not a whole address.
    const DIGITS: &[Kind] = &[Context, Namespace, Pod, Container, Section];
    let long = |n: usize| "a".repeat(n);
    let mut out = Vec::new();

    // A leading dash is an option of the tool: refused in every slot, and so are the options a card must never carry.
    for value in ["-", "--", "-v", "-n", "-1", "--raw", "-o", "-c", "--help", "-- x"] {
        out.push(refused_everywhere("leading_dash", value));
    }
    for (class, value) in [
        ("option_server", "--server=https://evil.example.invalid"),
        ("option_as", "--as=system:admin"),
        ("option_endpoints", "--endpoints=192.0.2.1"),
        ("option_subscription", "--subscription=11111111-2222-4333-8444-555555555555"),
        ("option_kubeconfig", "--kubeconfig=/etc/passwd"),
        ("option_token", "--token=abc"),
        ("option_talosconfig", "--talosconfig=/etc/passwd"),
        ("option_insecure", "--insecure-skip-tls-verify=true"),
        ("option_verbosity", "-v=9"),
    ] {
        out.push(refused_everywhere(class, value));
    }
    // An option behind a name is one value of the flag that carries it, and only a context may hold a space.
    for value in [
        "a --server=https://evil.example.invalid",
        "a --as=system:admin",
        "ctx --kubeconfig=/etc/passwd",
        "x --endpoints=192.0.2.1",
        "x -v=9",
        "a -- sh -c id",
    ] {
        out.push(context_only("inner_option", value));
    }
    // Words that the tools read as keywords, or that no list may hold.
    for value in ["save", "oauth", "select", "config", "verify", "todrive", "create", "update", "delete", "add", "remove"] {
        out.push(custom(
            "keyword",
            value,
            &[Context, Namespace, Pod, Container, Node],
            &[Section, Subscription, Lines, Flag],
        ));
    }
    for value in ["SAVE", "OAuth", "Select", "TODRIVE"] {
        out.push(context_only("keyword_upper", value));
    }
    for value in ["sh", "bash", "zsh", "get-access-token", "clear", "logout", "kubeconfig", "reset"] {
        out.push(custom("command_word", value, WORDS, NOT_WORDS));
    }
    // Controls and escape sequences.
    for code in [0x01, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x1b, 0x7f, 0x80, 0x85, 0x9b, 0x9f] {
        out.push(refused_everywhere("control", format!("a{}b", ch(code))));
    }
    out.push(refused_everywhere("control", ch(0x7f)));
    for value in ["\u{1b}[31mred", "\u{1b}]0;title\u{07}", "\u{1b}[2J", "\u{9b}31m", "a\u{1b}", "\u{1b}P1$r\u{1b}\\"] {
        out.push(refused_everywhere("escape_sequence", value));
    }
    // Direction marks, zero-width characters, format characters and tag characters.
    for code in [0x200e, 0x200f, 0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069, 0x061c] {
        out.push(refused_everywhere("direction", format!("a{}b", ch(code))));
    }
    out.push(refused_everywhere("direction", format!("{}evil", ch(0x202e))));
    for code in [0x200b, 0x200c, 0x200d, 0x2060, 0x2061, 0x2062, 0x2063, 0x2064, 0xfeff] {
        out.push(refused_everywhere("zero_width", format!("a{}b", ch(code))));
    }
    out.push(refused_everywhere("zero_width", ch(0x200b)));
    out.push(refused_everywhere("zero_width", format!("{}x", ch(0xfeff))));
    for code in [0x00ad, 0x180e, 0x206a, 0x206b, 0x206c, 0x206d, 0x206e, 0x206f, 0xfff9, 0xfffa, 0xfffb, 0x1d173, 0x1d17a] {
        out.push(refused_everywhere("format", format!("a{}b", ch(code))));
    }
    for code in [0x2028, 0x2029, 0x00a0, 0x2000, 0x2003, 0x2007, 0x202f, 0x205f, 0x3000, 0x1680] {
        out.push(refused_everywhere("space_like", format!("a{}b", ch(code))));
    }
    for code in [0xe0001, 0xe0020, 0xe0041, 0xe0061, 0xe007f] {
        out.push(refused_everywhere("tag", format!("a{}b", ch(code))));
    }
    out.push(refused_everywhere("tag", [0xe0041, 0xe0042].iter().map(|c| ch(*c)).collect::<String>()));
    // These look blank or combine with the character before: the invariant is asserted, the outcome is not.
    for code in [0x034f, 0x115f, 0x1160, 0x17b4, 0x17b5, 0x2800, 0x3164, 0xfe00, 0xfe0f, 0xffa0, 0xe0100] {
        out.push(any_outcome("blank_or_combining", format!("a{}b", ch(code))));
    }
    // NUL, empty and padded.
    for value in ["a\0b", "\0", "x\0", "\0x"] {
        out.push(refused_everywhere("nul", value));
    }
    out.push(refused_everywhere("empty", ""));
    for value in [" ", "  ", " lead", "trail ", " both ", "\u{20}x"] {
        out.push(refused_everywhere("padded", value));
    }
    for value in ["a b", "a  b", "two words here"] {
        out.push(context_only("inner_space", value));
    }
    // Lengths at the edges.
    out.push(refused_everywhere("length", long(254)));
    out.push(refused_everywhere("length", long(255)));
    out.push(refused_everywhere("length", long(4096)));
    out.push(refused_everywhere("length", long(4097)));
    out.push(refused_everywhere("length", long(70_000)));
    out.push(custom("length_edge", long(253), &[Context], &[Namespace, Pod, Container, Node, Section, Subscription, Lines, Flag]));
    out.push(custom("length_edge", long(65), &[Context], &[Namespace, Pod, Container, Node, Section, Subscription, Lines, Flag]));
    out.push(custom("length_edge", long(64), &[Context, Section], &[Namespace, Pod, Container, Node, Subscription, Lines, Flag]));
    out.push(custom("length_edge", long(63), WORDS, NOT_WORDS));
    // Paths and separators.
    for value in [
        "../../etc/passwd",
        "a/b",
        "a\\b",
        "C:\\Windows\\system32",
        "/etc/passwd",
        "~",
        "~/x",
        "..",
        ".",
        ".hidden",
        "a/../b",
        "\\\\server\\share",
        "file:///etc/passwd",
    ] {
        out.push(context_only("path", value));
    }
    // Text for a shell, a batch file and a template.
    for value in [
        "$(touch /tmp/never)",
        "`id`",
        "a;b",
        "a&&b",
        "a||b",
        "a|b",
        "a>b",
        "a<b",
        "%PATH%",
        "%0|%0",
        "a'b",
        "a\"b",
        "${IFS}",
        "$HOME",
        "*",
        "?",
        "a!b",
        "#",
        "a^b",
        "{{7*7}}",
        "a=b",
        "a,b",
        "key=value",
    ] {
        out.push(context_only("shell_meta", value));
    }
    for value in [
        "<img src=x onerror=alert(1)>",
        "<script>alert(1)</script>",
        "javascript:alert(1)",
        "a&amp;b",
        "\"><svg onload=alert(1)>",
        "data:text/html;base64,PHNjcmlwdD4=",
    ] {
        out.push(context_only("markup", value));
    }
    // Text that is printable and not ASCII.
    for value in ["\u{ff41}\u{ff42}\u{ff43}", "p\u{43e}d-0", "\u{540d}\u{524d}", "m\u{fc}nchen", "\u{1f600}", "e\u{301}", "\u{ff0d}\u{ff0d}server"] {
        out.push(any_outcome("non_ascii", value));
    }
    out.push(context_only("non_ascii_context", "\u{ff41}\u{ff42}\u{ff43}"));
    out.push(context_only("non_ascii_context", "p\u{43e}d-0"));
    // Shapes that are valid for one kind and not for another.
    out.push(context_only("valid_for_one", "arn:aws:eks:eu-central-1:000000000000:cluster/demo"));
    out.push(context_only("valid_for_one", "user@cluster.example:6443/ns"));
    out.push(custom("valid_for_one", "dev-cluster", WORDS, NOT_WORDS));
    out.push(custom("valid_for_one", "web-0.example", &[Context, Pod, Node, Section], &[Namespace, Container, Subscription, Lines, Flag]));
    out.push(custom("valid_for_one", "10.0.0.1", &[Context, Pod, Node, Section], &[Namespace, Container, Subscription, Lines, Flag]));
    out.push(custom("valid_for_one", "::1", &[Context, Node], &[Namespace, Pod, Container, Subscription, Section, Lines, Flag]));
    out.push(custom("valid_for_one", "2001:db8::1", &[Context, Node], &[Namespace, Pod, Container, Subscription, Section, Lines, Flag]));
    // Numbers for the line count and words for the flag.
    out.push(custom("number", "123", &[Lines, Context, Namespace, Pod, Container, Section], &[Node, Subscription, Flag]));
    out.push(custom("number", "5000", &[Lines, Context, Namespace, Pod, Container, Section], &[Node, Subscription, Flag]));
    out.push(custom("number", "1", &[Lines, Context, Namespace, Pod, Container, Section], &[Node, Subscription, Flag]));
    for value in ["0", "5001", "0100", "99999", "000"] {
        out.push(custom("number", value, DIGITS, &[Lines, Node, Subscription, Flag]));
    }
    for value in ["-1", "-0", "5 ", " 5"] {
        out.push(refused_everywhere("number", value));
    }
    for value in ["+5", "1.5", "\u{663}", "5\u{663}", "0x10"] {
        out.push(any_outcome("number", value));
    }
    out.push(custom("number", "1e3", WORDS, &[Lines, Subscription, Flag]));
    for value in ["true", "false"] {
        out.push(custom("flag", value, &[Flag, Context, Namespace, Pod, Container, Node, Section], &[Lines, Subscription]));
    }
    for value in ["TRUE", "False", "True"] {
        out.push(custom("flag", value, &[Context, Section], &[Flag, Lines, Subscription, Namespace, Pod, Container, Node]));
    }
    for value in ["yes", "on", "no", "maybe"] {
        out.push(custom("flag", value, WORDS, &[Flag, Lines, Subscription]));
    }
    for value in ["1", "0"] {
        out.push(custom("flag", value, DIGITS, &[Flag, Node, Subscription]));
    }
    for value in ["true ", " true", "true\n", "tru", "truee"] {
        out.push(any_outcome("flag", value));
    }
    // Subscriptions: a GUID, its variants, and a GUID with something behind it.
    out.push(custom(
        "guid",
        "11111111-2222-4333-8444-555555555555",
        &[Context, Namespace, Pod, Container, Node, Subscription, Section],
        &[Lines, Flag],
    ));
    out.push(custom(
        "guid",
        "ABCDEFAB-1234-4ABC-8DEF-ABCDEF123456",
        &[Context, Subscription, Section],
        &[Namespace, Pod, Container, Node, Lines, Flag],
    ));
    for value in [
        "11111111-2222-4333-8444-55555555555",
        "11111111-2222-4333-8444-5555555555555",
        "11111111222243338444555555555555",
        "GG111111-2222-4333-8444-555555555555",
        "11111111-2222-4333-8444-555555555555-",
        "my subscription",
        "Visual Studio Enterprise",
    ] {
        out.push(custom("guid_wrong", value, &[], &[Subscription, Lines, Flag]));
    }
    for value in [
        "{11111111-2222-4333-8444-555555555555}",
        "11111111-2222-4333-8444-555555555555&calc",
        "11111111-2222-4333-8444-555555555555 x",
        "11111111-2222-4333-8444-555555555555/../x",
    ] {
        out.push(context_only("guid_wrong", value));
    }
    // The names of a cluster that the validators must not reject.
    for value in ["prod-eu", "kind-test", "payments", "api-0", "default", "10.0.0.12", "kube-system", "a"] {
        out.push(custom("plain", value, &[Context], &[Subscription, Lines, Flag]));
    }
    let mut seen = BTreeSet::new();
    out.retain(|hostile| seen.insert(hostile.value.clone()));
    out
}

/// What a run of the corpus leaves behind.
#[derive(Default)]
struct Corpus {
    /// Every accepted request: the slot, the class, the value and the list the tool got.
    accepted: Vec<Accepted>,
    /// The baselines, one per slot and harmless value.
    baselines: Vec<(Slot, Vec<String>)>,
    classes: BTreeSet<&'static str>,
    slots: BTreeSet<String>,
    verbs: BTreeSet<String>,
    skipped: BTreeSet<String>,
    requests: usize,
    refused: usize,
    /// Every case that broke a rule, so that one run shows all of them.
    failures: Vec<String>,
}

struct Accepted {
    slot: Slot,
    class: &'static str,
    value: String,
    kind: Kind,
    list: Vec<String>,
}

fn run_corpus(tools: &[ToolId]) -> Corpus {
    let names = hostile_names();
    let parts: Vec<Corpus> = std::thread::scope(|scope| {
        let handles: Vec<_> = tools
            .iter()
            .map(|&tool| {
                let names = &names;
                scope.spawn(move || {
                    // A machine of its own: a start is recorded per fake, and the fakes of one machine are cleared together.
                    let m = Machine::new();
                    let mut part = Corpus::default();
                    let mut full_seen = std::collections::HashSet::new();
                    for mut slot in slots_of(tool) {
                        // The first verb that shows a selection field gets the whole corpus.
                        if let What::Select(field) = slot.what {
                            slot.full = full_seen.insert(field);
                        }
                        exercise_slot(&m, names, slot, &mut part);
                    }
                    part
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("a thread of the corpus")).collect()
    });
    let mut corpus = Corpus::default();
    for part in parts {
        corpus.accepted.extend(part.accepted);
        corpus.baselines.extend(part.baselines);
        corpus.classes.extend(part.classes);
        corpus.slots.extend(part.slots);
        corpus.verbs.extend(part.verbs);
        corpus.skipped.extend(part.skipped);
        corpus.requests += part.requests;
        corpus.refused += part.refused;
        corpus.failures.extend(part.failures);
    }
    corpus
}

fn exercise_slot(m: &Machine, names: &[Hostile], slot: Slot, corpus: &mut Corpus) {
    let (kind, token) = (slot.kind(), slot.token());
    // The baselines: the harmless value, or both words of a flag.
    let harmless: Vec<&str> = match kind {
        Kind::Profile => Vec::new(),
        Kind::Flag => vec!["true", "false"],
        _ => vec![token],
    };
    let mut baselines: Vec<(&str, Vec<String>)> = Vec::new();
    for value in harmless {
        let list = exercise(m, slot, value)
            .unwrap_or_else(|fault| panic!("the harmless value {value:?} was refused in {}: {fault:?}", slot.label()));
        baselines.push((value, list));
    }
    if kind != Kind::Profile && kind != Kind::Flag {
        let shown = baselines[0].1.iter().any(|element| element.contains(token));
        if !shown {
            // The selection field is not part of this verb's list (a read of the contexts, a verb with an argument of
            // its own that wins). An argument must always show.
            assert!(!matches!(slot.what, What::Arg(_)), "the harmless argument is not in the list of {}", slot.label());
            corpus.skipped.insert(slot.label());
            return;
        }
    }
    corpus.slots.insert(slot.label());
    corpus.verbs.insert(format!("{}/{}", slot.tool.as_str(), slot.verb.id));
    for (_, list) in &baselines {
        corpus.baselines.push((slot, list.clone()));
    }
    let mut in_class: BTreeMap<&str, usize> = BTreeMap::new();
    for hostile in names {
        let nth = in_class.entry(hostile.class).or_default();
        *nth += 1;
        corpus.classes.insert(hostile.class);
        // A sample: the first value of each class, then every fourth.
        if !slot.full && (*nth - 1) % 4 != 0 {
            continue;
        }
        corpus.requests += 1;
        let here = format!("{} with {:?} ({})", slot.label(), hostile.value, hostile.class);
        let got = exercise(m, slot, &hostile.value);
        if (kind == Kind::Profile || hostile.refuse.contains(&kind)) && got.is_ok() {
            corpus.failures.push(format!("accepted but must be refused: {here}"));
        }
        if hostile.accept.contains(&kind) && got.is_err() {
            corpus.failures.push(format!("refused but must be accepted: {here}"));
        }
        match got {
            Err(fault) => {
                assert_eq!(fault.code, FaultCode::Refused, "{here}");
                corpus.refused += 1;
            }
            Ok(list) => {
                let expected: Vec<String> = if kind == Kind::Flag {
                    let (_, base) = baselines
                        .iter()
                        .find(|(word, _)| *word == hostile.value)
                        .unwrap_or_else(|| panic!("a flag took a value that is not true or false: {here}"));
                    base.clone()
                } else {
                    let canon = canonical(kind, &hostile.value);
                    baselines[0].1.iter().map(|element| element.replace(token, &canon)).collect()
                };
                if list != expected {
                    corpus.failures.push(format!("not the harmless list with the name in its place: {here}: {list:?}"));
                }
                corpus.accepted.push(Accepted {
                    slot,
                    class: hostile.class,
                    value: hostile.value.clone(),
                    kind,
                    list,
                });
            }
        }
    }
}

fn corpus() -> &'static Corpus {
    static CORPUS: OnceLock<Corpus> = OnceLock::new();
    CORPUS.get_or_init(|| run_corpus(&ToolId::ALL))
}

fn report(corpus: &Corpus) {
    println!(
        "Injection corpus: {} classes, {} verbs, {} slots ({} skipped as not part of the verb), {} requests, {} refused, {} accepted",
        corpus.classes.len(),
        corpus.verbs.len(),
        corpus.slots.len(),
        corpus.skipped.len(),
        corpus.requests,
        corpus.refused,
        corpus.accepted.len()
    );
    let mut per_tool: BTreeMap<&str, usize> = BTreeMap::new();
    for label in &corpus.slots {
        *per_tool.entry(label.split('/').next().unwrap_or("")).or_default() += 1;
    }
    println!("Injection slots per tool: {per_tool:?}");
    let mut per_class: BTreeMap<&str, usize> = BTreeMap::new();
    for accepted in &corpus.accepted {
        *per_class.entry(accepted.class).or_default() += 1;
    }
    println!("Injection accepted per class: {per_class:?}");
}

// ---- the tests -----------------------------------------------------------------------------------------------------

#[test]
fn a_hostile_name_is_refused_before_a_process_or_recorded_as_one_value() {
    let corpus = corpus();
    report(corpus);
    assert!(corpus.failures.is_empty(), "{} cases broke a rule:\n{}", corpus.failures.len(), corpus.failures.join("\n"));
    assert!(corpus.classes.len() >= 12, "at least 12 classes of hostile names, got {}", corpus.classes.len());
    assert!(corpus.slots.len() >= 25, "at least 25 slots, got {}", corpus.slots.len());
    assert!(corpus.refused > 0 && !corpus.accepted.is_empty(), "both outcomes were run");
    for tool in ToolId::ALL {
        let own = corpus.slots.iter().filter(|label| label.starts_with(tool.as_str())).count();
        assert!(own >= 3, "{} has slots: {own}", tool.as_str());
    }
}

// ---- the sweeps over every list ------------------------------------------------------------------------------------

/// Flags that give a tool a credential, another cluster, more output or a bypass of its checks: none may be in a list.
const FORBIDDEN_FLAGS: &[&str] = &[
    "--raw",
    "--token",
    "--kubeconfig",
    "--server",
    "--insecure-skip-tls-verify",
    "--as",
    "--as-group",
    "--as-uid",
    "--talosconfig",
    "--endpoints",
    "-e",
    "--refresh",
    "--debug",
    "--verbose",
    "--verbosity",
    "-v",
    "--v",
    "--password",
    "--username",
    "--client-key",
    "--client-certificate",
    "--certificate-authority",
    "--cluster",
    "--user",
    "--tls-server-name",
    "--proxy-url",
    "--cache-dir",
];

/// Words that are a sub-command no list of the integrations holds, per tool. A name that is one of them is a value in
/// the place of a name and is skipped by the sweep; every other element is looked at.
fn forbidden_words(tool: ToolId) -> &'static [&'static str] {
    match tool {
        ToolId::Kube => &["proxy", "apply", "edit", "patch", "replace", "cp", "attach", "port-forward", "auth", "create", "drain"],
        ToolId::Talos => &["reset", "upgrade", "reboot", "shutdown", "apply-config", "edit", "patch", "kubeconfig", "bootstrap", "wipe"],
        ToolId::Azure => &["get-access-token", "clear", "logout", "delete", "create", "update"],
        ToolId::Gam => &["save", "oauth", "create", "update", "delete", "add", "remove", "todrive"],
    }
}

/// Words of a shell: no element of a list is one, except the three of the pod exec.
const SHELL_WORDS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ksh", "csh", "tcsh", "cmd", "cmd.exe", "powershell", "pwsh", "-c", "-lc", "/bin/sh",
    "/bin/bash", "eval", "&&", "||", ";", "|",
];

/// The part of an element that names a flag: the text before the first `=`, for an element that starts with a dash.
fn flag_name(element: &str) -> Option<&str> {
    element.starts_with('-').then(|| element.split_once('=').map_or(element, |(name, _)| name))
}

/// Checks a list a tool was given. `value` is the name that was put in it, or `None` for a baseline: an element that is
/// that name, or a flag that carries it, is data and not structure.
fn sweep(tool: ToolId, verb: &str, list: &[String], value: Option<&str>) -> Result<(), String> {
    // The pod exec ends in `-- sh -c <constant>`: that tail is the one place a shell word may be, and nothing else of it.
    let exec_tail = tool == ToolId::Kube
        && verb == "exec"
        && list.len() >= 4
        && list[list.len() - 4..] == ["--", "sh", "-c", POD_SHELL];
    let structure = if exec_tail { &list[..list.len() - 3] } else { list };
    for element in structure {
        if let Some(name) = flag_name(element) {
            if FORBIDDEN_FLAGS.contains(&name) {
                return Err(format!("forbidden flag {name:?} in {tool:?} {verb}: {list:?}"));
            }
        }
        let is_data = value.is_some_and(|value| element == value || element.ends_with(&format!("={value}")));
        if is_data {
            continue;
        }
        if forbidden_words(tool).contains(&element.as_str()) && !is_a_verb_word(tool, verb, element) {
            return Err(format!("forbidden word {element:?} in {tool:?} {verb}: {list:?}"));
        }
        if SHELL_WORDS.contains(&element.as_str()) {
            return Err(format!("shell word {element:?} in {tool:?} {verb}: {list:?}"));
        }
        if element.chars().any(|c| matches!(c, ';' | '|' | '&' | '`' | '$' | '<' | '>' | '\n')) {
            return Err(format!("a shell character in the constant part {element:?} of {tool:?} {verb}: {list:?}"));
        }
    }
    Ok(())
}

/// The verbs whose own list holds a word of the forbidden ones: deleting a pod is `delete`, and nothing else.
fn is_a_verb_word(tool: ToolId, verb: &str, element: &str) -> bool {
    matches!((tool, verb, element), (ToolId::Kube, "delete_pod", "delete"))
}

#[test]
fn no_recorded_list_holds_a_forbidden_flag_or_a_shell_word() {
    let corpus = corpus();
    assert!(corpus.baselines.len() >= 25, "the baselines were swept: {}", corpus.baselines.len());
    for (slot, list) in &corpus.baselines {
        sweep(slot.tool, slot.verb.id, list, None).unwrap_or_else(|why| panic!("{why}"));
    }
    for accepted in &corpus.accepted {
        let value = canonical(accepted.kind, &accepted.value);
        sweep(accepted.slot.tool, accepted.slot.verb.id, &accepted.list, Some(&value)).unwrap_or_else(|why| panic!("{why}"));
    }
    // The sweep has teeth: each of these lists must fail it.
    let bad: [(ToolId, &str, Vec<&str>); 7] = [
        (ToolId::Kube, "pods", vec!["get", "pods", "--raw"]),
        (ToolId::Kube, "pods", vec!["get", "pods", "--server=https://evil.example.invalid"]),
        (ToolId::Kube, "pods", vec!["get", "pods", "-v=9"]),
        (ToolId::Kube, "pods", vec!["get", "pods", "sh", "-c", "id"]),
        (ToolId::Talos, "nodes", vec!["get", "members", "--endpoints=192.0.2.1"]),
        (ToolId::Azure, "account", vec!["account", "get-access-token"]),
        (ToolId::Gam, "users", vec!["print", "users", "save"]),
    ];
    for (tool, verb, list) in bad {
        let list: Vec<String> = list.into_iter().map(str::to_owned).collect();
        assert!(sweep(tool, verb, &list, None).is_err(), "the sweep must refuse {list:?}");
    }
    // The pod exec's tail is the one shell word that passes, and only at the end.
    let exec: Vec<String> = ["exec", "-i", "-t", "--context=a", "pod-0", "--", "sh", "-c", POD_SHELL]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert_eq!(sweep(ToolId::Kube, "exec", &exec, None), Ok(()));
    assert!(sweep(ToolId::Kube, "logs", &exec, None).is_err(), "the tail is allowed for the exec only");
    let mut extra = exec.clone();
    extra.push("id".to_owned());
    assert!(sweep(ToolId::Kube, "exec", &extra, None).is_err(), "and only as the end of the list");
}

#[test]
fn a_value_valid_for_one_kind_is_accepted_only_where_its_kind_allows_it() {
    let corpus = corpus();
    for sample in ["arn:aws:eks:eu-central-1:000000000000:cluster/demo", "a b", "user@cluster.example:6443/ns"] {
        let accepted: Vec<&Accepted> = corpus.accepted.iter().filter(|a| a.value == sample).collect();
        assert!(!accepted.is_empty(), "{sample:?} is a context somewhere");
        for a in accepted {
            assert_eq!(a.kind, Kind::Context, "{sample:?} was accepted as {:?} in {}", a.kind, a.slot.label());
            // Another tool's list is not changed in shape: the same verb gives the same number of elements with a
            // harmless name (the equality with the baseline was asserted when it was recorded).
            let base = corpus
                .baselines
                .iter()
                .find(|(slot, _)| slot.label() == a.slot.label())
                .map(|(_, list)| list.len())
                .expect("a baseline for the slot");
            assert_eq!(a.list.len(), base, "{sample:?} changed the shape of {}", a.slot.label());
        }
    }
    // The same names are refused in the slots of the other kinds, in every tool.
    let wrong_kind = corpus
        .accepted
        .iter()
        .filter(|a| a.value.contains(':') && !matches!(a.kind, Kind::Context | Kind::Node))
        .count();
    assert_eq!(wrong_kind, 0, "a name with a colon is a context or a node (an address), never another kind");
}

// ---- the closed catalogue and the selection reader ------------------------------------------------------------------

#[test]
fn a_hostile_name_in_the_place_of_a_tool_a_verb_or_an_argument_name_is_refused() {
    let m = Machine::new();
    let names = hostile_names();
    let mut run = 0_usize;
    let none = BTreeMap::new();
    for hostile in &names {
        let value = hostile.value.as_str();
        // As the tool: every call refuses it.
        let faults = [
            m.service.run_view(value, "pods", None, &none).err(),
            m.service.prepare(value, "delete_pod", None, &none).err(),
            m.service.launch(value, "logs", None, &none).err(),
            m.service.select(value, Some(1), &Selection::default()).err(),
            m.service.open_pane(1, value, "pods").err(),
        ];
        for fault in faults {
            let fault = fault.unwrap_or_else(|| panic!("{value:?} was taken for a tool"));
            assert_eq!(fault.code, FaultCode::Refused, "{value:?}");
            assert!(!fault.text.contains(value) || value.len() < 3, "the refusal repeats the input: {:?}", fault.text);
            run += 1;
        }
        for tool in ToolId::ALL {
            let name = tool.as_str();
            // As the verb, in each lane.
            for fault in [
                m.service.run_view(name, value, None, &none).err(),
                m.service.prepare(name, value, None, &none).err(),
                m.service.launch(name, value, None, &none).err(),
            ] {
                let fault = fault.unwrap_or_else(|| panic!("{value:?} was taken for a verb of {name}"));
                assert_eq!(fault.code, FaultCode::Refused, "{value:?}");
                run += 1;
            }
            // As the name of an argument, for every verb in its own lane.
            for verb in spec_of(tool).verbs {
                let mut args = BTreeMap::new();
                for own in verb.args {
                    args.insert(own.name.to_owned(), harmless_arg(own.kind).to_owned());
                }
                args.insert(value.to_owned(), "x".to_owned());
                let fault = call(&m, tool, verb, &args, None).expect_err("an argument that is not declared");
                assert_eq!(fault.code, FaultCode::Refused, "{value:?} as an argument of {name} {}", verb.id);
                run += 1;
            }
        }
    }
    assert!(!m.any_started(), "nothing was started by a request in the wrong place");
    println!("Injection structure: {run} requests with a hostile tool, verb or argument name, all refused, none started");
    assert!(run >= 1000, "{run} requests");
}

#[test]
fn a_selection_that_is_not_an_object_of_known_fields_is_not_read() {
    let hostile = [
        r#"{"context":"a","context":"b"}"#,
        r#"["a","b","c"]"#,
        r#"{"--server":"x"}"#,
        r#"{"server":"https://evil.example.invalid"}"#,
        r#"{"context":"a","kubeconfig":"/etc/passwd"}"#,
        r#""context""#,
        "null",
        "42",
    ];
    for text in hostile {
        let error = serde_json::from_str::<Selection>(text).expect_err(text);
        let message = error.to_string();
        assert!(!message.contains("evil.example") && !message.contains("/etc/passwd"), "the message repeats the input: {message}");
    }
    let fine = serde_json::from_str::<Selection>(r#"{"context":"a","namespace":"b"}"#).expect("an object of fields");
    assert_eq!((fine.context.as_deref(), fine.namespace.as_deref()), (Some("a"), Some("b")));
    // A field that the tool does not select is refused before its value is looked at, for every tool and every field.
    let m = Machine::new();
    for tool in ToolId::ALL {
        for field in [SelectKind::Context, SelectKind::Namespace, SelectKind::Subscription, SelectKind::Section, SelectKind::Node] {
            if tool.selects().contains(&field) {
                continue;
            }
            for value in ["ok", "--server=x", "a\u{202e}b", ""] {
                let fault = m
                    .service
                    .select(tool.as_str(), Some(next_pane()), &selection_with(field, value))
                    .expect_err("a field of another tool");
                assert_eq!(fault.code, FaultCode::Refused, "{field:?} for {tool:?}");
            }
        }
    }
    assert!(!m.any_started());
}
