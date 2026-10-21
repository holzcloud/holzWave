//! The Azure command-line tool: the argument lists of its verbs, the parsers of its answers and the sign-in state.
//!
//! `az` is a Python program that talks to Microsoft and keeps its own tokens in its configuration directory, so this
//! module holds only what is safe to hold: argument builders and parsers. It starts no process, reads no file and
//! keeps no state; the generic engine runs the tool.
//!
//! The rules of the tool, each one measured:
//!
//! - Every read asks for `-o json --only-show-errors`. A parameter on the command line beats the person's configured
//!   output format (a `core.output=table` setting would break every parser) and beats the environment, and the update
//!   notices and warnings that the tool prints on its error stream are left out. Only standard output is parsed.
//! - Being signed out is a normal state and not an error: `account show` then exits with a non-zero status, writes
//!   nothing to standard output and says on the error stream that a sign-in is needed. The verb is declared with
//!   `exit_is_state`, the empty output becomes the state `Signed out` with a `login` action, and [`login_state`] reads
//!   the same output into [`LoginState`], so the view, the status segment and the context section cannot disagree.
//! - A subscription is addressed by its GUID and never by its display name, in the equals form
//!   (`--subscription=<guid>`), so the argument is one element that cannot become an option. On Windows the tool is
//!   a batch file (`az.cmd`), which the command interpreter parses: a GUID is made of characters that the strict
//!   batch-file charset of the runner accepts, and the argument lists built here hold nothing else but constants of
//!   that charset.
//! - `account set` writes the tool's own `azureProfile.json` in the configuration directory of the person. That is
//!   why it is a `Local` verb that goes through a card naming the Azure CLI configuration of this computer. No
//!   subscription flag is a substitute for it: the tool has no environment variable that selects a subscription, so
//!   every block names its subscription on its own command line and nothing on disk changes.
//! - `login` opens a browser, or prints a device code, in a terminal block: it is `Exec` and gets no flag (no tenant,
//!   no device-code switch), so the terminal shows what the tool prints and the person completes the sign-in.
//! - Never built here: an access-token request, `account clear`, a refresh switch, a verbose or debug switch, a query
//!   expression, another output format. The typed structs below have no member for a token or a secret, and the tag
//!   values of a resource group are never read, so none of them can reach a model.
//! - Telemetry is switched off for every run holzWave starts through the one variable of the spec's `env_set`. The
//!   person's own use of the tool in a terminal is not touched. `AZURE_CORE_DISABLE_CONFIRM_PROMPT` is never set and
//!   standard input is closed by the runner, so nothing can wait for an answer.
//!
//! Every struct read from the tool's JSON goes through [`object_only`]: a derived reader also accepts a JSON array as
//! the list of member values in order, which is no answer of this tool. Members the module does not show are skipped
//! without being stored, and a member of the wrong type reads as absent and shows as `?`.

use std::ffi::OsString;
use std::fmt;
use std::marker::PhantomData;

use serde::de::value::MapAccessDeserializer;
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

use super::args::{flag_eq, ArgError, SubscriptionId};
use super::view::{clean_cell, text_fallback, KvBuilder, RowBuilder, TableBuilder, Tone, ViewModel};
use super::{
    ArgKind, ArgSpec, CheckedSelection, DefaultRead, Integration, IntegrationSpec, Output, ParseError, SelectKind,
    Selection, ToolId, Validated, Verb, VerbEffect, VersionProbe,
};

/// What a cell shows for a member that is missing or of the wrong type.
const UNKNOWN: &str = "?";
/// The most bytes of a user or a subscription name the sign-in state keeps; the status line shows less.
const LOGIN_FIELD_MAX: usize = 256;
/// The most bytes of a document that are parsed; the runner's own cap is larger, so a bigger answer is no answer.
const READ_MAX: usize = 8 * 1024 * 1024;
/// The longest version text accepted.
const VERSION_MAX: usize = 64;

/// The arguments every read adds: the format on the command line, and no notices.
const JSON_ONLY: [&str; 3] = ["-o", "json", "--only-show-errors"];

const SUBSCRIPTION: ArgSpec = ArgSpec { name: "subscription", kind: ArgKind::Subscription, required: true };

/// The closed catalogue.
const VERBS: [Verb; 5] = [
    Verb {
        id: "account",
        label: "Account",
        effect: VerbEffect::Read,
        output: Output::View,
        args: &[],
        refresh_secs: None,
        exit_is_state: true,
    },
    Verb {
        id: "subscriptions",
        label: "Subscriptions",
        effect: VerbEffect::Read,
        output: Output::View,
        args: &[],
        refresh_secs: None,
        exit_is_state: false,
    },
    Verb {
        id: "groups",
        label: "Resource groups",
        effect: VerbEffect::Read,
        output: Output::View,
        args: &[],
        refresh_secs: None,
        exit_is_state: false,
    },
    Verb {
        id: "set_subscription",
        label: "Make default",
        effect: VerbEffect::Local,
        output: Output::Done,
        args: &[SUBSCRIPTION],
        refresh_secs: None,
        exit_is_state: false,
    },
    Verb {
        id: "login",
        label: "Sign in",
        effect: VerbEffect::Exec,
        output: Output::Terminal,
        args: &[],
        refresh_secs: None,
        exit_is_state: false,
    },
];

/// The description of the tool.
pub static SPEC: IntegrationSpec = IntegrationSpec {
    id: ToolId::Azure,
    label: "Azure",
    binaries: &["az"],
    windows_binaries: &["az.cmd", "az.exe"],
    fixed_dirs: &[
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/snap/bin",
        "/home/linuxbrew/.linuxbrew/bin",
        "$HOME/.local/bin",
        "%ProgramFiles%\\Microsoft SDKs\\Azure\\CLI2\\wbin",
        "%LOCALAPPDATA%\\Microsoft\\WinGet\\Links",
    ],
    version_probe: VersionProbe { argv: &["version", "-o", "json"], parse: parse_version },
    env_set: &[("AZURE_CORE_COLLECT_TELEMETRY", "false")],
    selects: &[SelectKind::Subscription],
    verbs: &VERBS,
};

// ---------------------------------------------------------------------------------------------------------------------
// Readers. A struct is read from a JSON object only; a member that is not what it should be reads as absent.
// ---------------------------------------------------------------------------------------------------------------------

/// Reads a struct from a JSON object only. A derived `Deserialize` also reads an array of the member values, in order;
/// the tool never writes that, so it is refused.
fn object_only<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<T, D::Error> {
    struct Members<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for Members<T> {
        type Value = T;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an object")
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(Members(PhantomData))
}

/// Skips the rest of a sequence or a map without keeping a byte of it.
fn drain_seq<'de, A: SeqAccess<'de>>(mut seq: A) -> Result<(), A::Error> {
    while seq.next_element::<IgnoredAny>()?.is_some() {}
    Ok(())
}

fn drain_map<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
    Ok(())
}

/// A text member: the text, or `None` for a member of any other type. The value of another type is skipped.
fn soft_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    struct Text;

    impl<'de> Visitor<'de> for Text {
        type Value = Option<String>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("any value")
        }

        fn visit_str<E>(self, text: &str) -> Result<Self::Value, E> {
            Ok(Some(text.to_owned()))
        }

        fn visit_string<E>(self, text: String) -> Result<Self::Value, E> {
            Ok(Some(text))
        }

        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            drain_seq(seq).map(|()| None)
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
            drain_map(map).map(|()| None)
        }
    }

    deserializer.deserialize_any(Text)
}

/// A flag member: the value, or `None` for a member of any other type.
fn soft_bool<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<bool>, D::Error> {
    struct Flag;

    impl<'de> Visitor<'de> for Flag {
        type Value = Option<bool>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("any value")
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
            Ok(Some(value))
        }

        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            drain_seq(seq).map(|()| None)
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
            drain_map(map).map(|()| None)
        }
    }

    deserializer.deserialize_any(Flag)
}

/// An object member: the struct, or `None` for a member of any other type (an array included).
fn soft_object<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    struct Member<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for Member<T> {
        type Value = Option<T>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("any value")
        }

        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            drain_seq(seq).map(|()| None)
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
            T::deserialize(MapAccessDeserializer::new(map)).map(Some)
        }
    }

    deserializer.deserialize_any(Member(PhantomData))
}

/// The number of members of an object member (null counts as none), or `None` for a member of any other type. The keys
/// and the values are skipped without being read, so no tag of a resource group is ever held.
fn soft_count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u32>, D::Error> {
    struct Count;

    impl<'de> Visitor<'de> for Count {
        type Value = Option<u32>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("any value")
        }

        fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(Some(0))
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(Some(0))
        }

        fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            drain_seq(seq).map(|()| None)
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            let mut count: u32 = 0;
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {
                count = count.saturating_add(1);
            }
            Ok(Some(count))
        }
    }

    deserializer.deserialize_any(Count)
}

/// Without a leading byte order mark, which a tool on Windows may write.
fn without_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
}

fn is_blank(bytes: &[u8]) -> bool {
    without_bom(bytes).iter().all(u8::is_ascii_whitespace)
}

/// A cleaned text cut to `max` bytes on a character boundary, ending in an ellipsis when it was cut.
fn bounded(text: &str, max: usize) -> String {
    let clean = clean_cell(text);
    if clean.len() <= max {
        return clean;
    }
    let mut end = max.saturating_sub('\u{2026}'.len_utf8());
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    let mut cut = clean[..end].to_owned();
    cut.push('\u{2026}');
    cut
}

// ---------------------------------------------------------------------------------------------------------------------
// The account.
// ---------------------------------------------------------------------------------------------------------------------

/// The members of `account show` that are shown. There is no member for a token, a tenant secret or the list of
/// managing tenants.
#[derive(Deserialize)]
struct RawUser {
    #[serde(default, deserialize_with = "soft_text")]
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawAccount {
    #[serde(default, deserialize_with = "soft_text")]
    id: Option<String>,
    #[serde(default, deserialize_with = "soft_text")]
    name: Option<String>,
    #[serde(default, deserialize_with = "soft_text")]
    state: Option<String>,
    #[serde(default, rename = "tenantId", deserialize_with = "soft_text")]
    tenant_id: Option<String>,
    #[serde(default, rename = "environmentName", alias = "cloudName", deserialize_with = "soft_text")]
    environment: Option<String>,
    #[serde(default, deserialize_with = "soft_object")]
    user: Option<RawUser>,
}

struct AccountDoc(RawAccount);

impl<'de> Deserialize<'de> for AccountDoc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        object_only(deserializer).map(Self)
    }
}

/// What the tool said about the account, once.
enum Account {
    SignedOut,
    SignedIn(Box<Profile>),
    Unknown,
}

/// A signed-in account: the subscription id is a valid GUID, the rest is cleaned and bounded text or absent.
struct Profile {
    id: SubscriptionId,
    name: Option<String>,
    state: Option<String>,
    tenant: Option<String>,
    environment: Option<String>,
    user: Option<String>,
}

/// The one parser of `account show`: empty (or blank) output is signed out, an object with a subscription GUID is
/// signed in, anything else is not understood.
fn read_account(stdout: &[u8]) -> Account {
    if is_blank(stdout) {
        return Account::SignedOut;
    }
    if stdout.len() > READ_MAX {
        return Account::Unknown;
    }
    let Ok(AccountDoc(raw)) = serde_json::from_slice::<AccountDoc>(without_bom(stdout)) else {
        return Account::Unknown;
    };
    let Some(id) = raw.id.as_deref().and_then(|id| SubscriptionId::parse(id).ok()) else {
        return Account::Unknown;
    };
    let text = |value: Option<String>| value.map(|value| bounded(&value, LOGIN_FIELD_MAX));
    Account::SignedIn(Box::new(Profile {
        id,
        name: text(raw.name),
        state: text(raw.state),
        tenant: text(raw.tenant_id),
        environment: text(raw.environment),
        user: text(raw.user.and_then(|user| user.name)),
    }))
}

/// Where the person stands with the tool, as the status segment and the context section read it.
#[derive(Clone, PartialEq, Eq)]
pub enum LoginState {
    /// A subscription is selected for the person's account.
    SignedIn {
        /// The user name (an address or an application id), cleaned and bounded; `?` when the tool gave none.
        user: String,
        /// The default subscription, a valid GUID.
        subscription_id: SubscriptionId,
        /// The display name of the subscription, cleaned and bounded; `?` when the tool gave none.
        subscription_name: String,
    },
    /// The tool says no account is signed in.
    SignedOut,
    /// The output is neither of the two.
    Unknown,
}

impl fmt::Debug for LoginState {
    /// The variant and the sizes only: a user name is personal data and does not belong in a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SignedIn { user, subscription_name, .. } => {
                write!(f, "SignedIn(user len={}, subscription name len={})", user.len(), subscription_name.len())
            }
            Self::SignedOut => f.write_str("SignedOut"),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}

/// The sign-in state in the standard output of `account show`: empty output is `SignedOut`, an account with a
/// subscription GUID is `SignedIn`, everything else is `Unknown`. The view of the `account` verb reads the same
/// output through the same parser.
pub fn login_state(stdout: &[u8]) -> LoginState {
    match read_account(stdout) {
        Account::SignedOut => LoginState::SignedOut,
        Account::SignedIn(profile) => {
            let Profile { id, name, user, .. } = *profile;
            LoginState::SignedIn {
                user: user.unwrap_or_else(|| UNKNOWN.to_owned()),
                subscription_id: id,
                subscription_name: name.unwrap_or_else(|| UNKNOWN.to_owned()),
            }
        }
        Account::Unknown => LoginState::Unknown,
    }
}

/// The key-value view of the account: signed out with a `login` action, signed in with what is known, or the text of
/// an output that is neither.
fn account_view(stdout: &[u8]) -> ViewModel {
    let mut view = KvBuilder::new("Azure account");
    match read_account(stdout) {
        Account::SignedOut => {
            view.row("State", "Signed out", Tone::Warn);
            if let Some(login) = SPEC.verb("login") {
                view.action(login, &[]);
            }
        }
        Account::SignedIn(profile) => {
            let shown = |value: &Option<String>| value.clone().unwrap_or_else(|| UNKNOWN.to_owned());
            let state = shown(&profile.state);
            let tone = if state == UNKNOWN || state == "Enabled" { Tone::Normal } else { Tone::Warn };
            view.row("State", "Signed in", Tone::Normal)
                .row("User", &shown(&profile.user), Tone::Normal)
                .row("Subscription", &shown(&profile.name), Tone::Normal)
                .row("Subscription ID", profile.id.as_str(), Tone::Normal)
                .row("Tenant ID", &shown(&profile.tenant), Tone::Normal)
                .row("Environment", &shown(&profile.environment), Tone::Normal)
                .row("Subscription state", &state, tone);
        }
        Account::Unknown => return text_fallback("Azure account", stdout),
    }
    view.finish()
}

// ---------------------------------------------------------------------------------------------------------------------
// Subscriptions.
// ---------------------------------------------------------------------------------------------------------------------

/// The members of an entry of `account list` that are shown. The tenant lists, the cloud and the user are dropped.
#[derive(Deserialize)]
struct RawSubscription {
    #[serde(default, deserialize_with = "soft_text")]
    id: Option<String>,
    #[serde(default, deserialize_with = "soft_text")]
    name: Option<String>,
    #[serde(default, deserialize_with = "soft_text")]
    state: Option<String>,
    #[serde(default, rename = "tenantId", deserialize_with = "soft_text")]
    tenant_id: Option<String>,
    #[serde(default, rename = "isDefault", deserialize_with = "soft_bool")]
    is_default: Option<bool>,
}

struct SubscriptionDoc(RawSubscription);

impl<'de> Deserialize<'de> for SubscriptionDoc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        object_only(deserializer).map(Self)
    }
}

/// How a read of a list ended.
enum Listing<T> {
    /// A list of objects.
    Items(Vec<T>),
    /// Output that is no list of objects: the text of it is shown.
    Other,
}

/// The list in the standard output of a read verb. No output at all is [`ParseError::Empty`], an answer over
/// [`READ_MAX`] is [`ParseError::TooLarge`].
fn read_list<T: for<'de> Deserialize<'de>>(stdout: &[u8]) -> Result<Listing<T>, ParseError> {
    if is_blank(stdout) {
        return Err(ParseError::Empty);
    }
    if stdout.len() > READ_MAX {
        return Err(ParseError::TooLarge);
    }
    Ok(serde_json::from_slice::<Vec<T>>(without_bom(stdout)).map_or(Listing::Other, Listing::Items))
}

fn shown(value: Option<&str>) -> &str {
    value.unwrap_or(UNKNOWN)
}

/// A table of the subscriptions of the account: a star on the default, the state in words, and a button to make one
/// the default for each entry whose id is a GUID (an id of another shape is shown and has none).
fn subscriptions_view(stdout: &[u8]) -> Result<ViewModel, ParseError> {
    const TITLE: &str = "Azure subscriptions";
    let Listing::Items(items) = read_list::<SubscriptionDoc>(stdout)? else {
        return Ok(text_fallback(TITLE, stdout));
    };
    let mut table = TableBuilder::new(TITLE);
    table.column("default", "Default").column("name", "Name").column("id", "ID").column("state", "State").column(
        "tenant",
        "Tenant",
    );
    let make_default = SPEC.verb("set_subscription");
    for SubscriptionDoc(item) in &items {
        let guid = item.id.as_deref().and_then(|id| SubscriptionId::parse(id).ok());
        let id = guid.as_ref().map(SubscriptionId::as_str).or(item.id.as_deref());
        let mark = if item.is_default == Some(true) { "*" } else { "" };
        let cells = [
            mark,
            shown(item.name.as_deref()),
            shown(id),
            shown(item.state.as_deref()),
            shown(item.tenant_id.as_deref()),
        ];
        let mut row = RowBuilder::new(shown(id), &cells);
        // Only the state decides the tone: the star is the cue for the default.
        if item.state.as_deref().is_some_and(|state| state != "Enabled") {
            row.tone(Tone::Warn);
        }
        if let (Some(guid), Some(verb)) = (&guid, make_default) {
            row.action(verb, &[("subscription", guid.as_str())]);
        }
        table.push(row);
    }
    Ok(table.finish())
}

// ---------------------------------------------------------------------------------------------------------------------
// Resource groups.
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawProperties {
    #[serde(default, rename = "provisioningState", deserialize_with = "soft_text")]
    state: Option<String>,
}

/// The members of an entry of `group list` that are shown. The tags are counted and nothing else: a tag is text that
/// an account owner or somebody else chose.
#[derive(Deserialize)]
struct RawGroup {
    #[serde(default, deserialize_with = "soft_text")]
    name: Option<String>,
    #[serde(default, deserialize_with = "soft_text")]
    location: Option<String>,
    #[serde(default, deserialize_with = "soft_object")]
    properties: Option<RawProperties>,
    #[serde(default, deserialize_with = "soft_count")]
    tags: Option<u32>,
}

struct GroupDoc(RawGroup);

impl<'de> Deserialize<'de> for GroupDoc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        object_only(deserializer).map(Self)
    }
}

/// A table of the resource groups of the selected (or the default) subscription: name, location, provisioning state
/// and the number of tags. A state other than `Succeeded` is told in the tone as well as in the word.
fn groups_view(stdout: &[u8]) -> Result<ViewModel, ParseError> {
    const TITLE: &str = "Azure resource groups";
    let Listing::Items(items) = read_list::<GroupDoc>(stdout)? else {
        return Ok(text_fallback(TITLE, stdout));
    };
    let mut table = TableBuilder::new(TITLE);
    table.column("name", "Name").column("location", "Location").column("state", "State").column("tags", "Tags");
    for GroupDoc(item) in &items {
        let state = item.properties.as_ref().and_then(|properties| properties.state.as_deref());
        let tags = item.tags.map(|count| count.to_string());
        let mut row = RowBuilder::new(
            shown(item.name.as_deref()),
            &[shown(item.name.as_deref()), shown(item.location.as_deref()), shown(state), shown(tags.as_deref())],
        );
        if state.is_some_and(|state| state != "Succeeded") {
            row.tone(Tone::Warn);
        }
        table.push(row);
    }
    Ok(table.finish())
}

// ---------------------------------------------------------------------------------------------------------------------
// The version.
// ---------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct RawVersion {
    #[serde(default, rename = "azure-cli", deserialize_with = "soft_text")]
    version: Option<String>,
}

struct VersionDoc(RawVersion);

impl<'de> Deserialize<'de> for VersionDoc {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        object_only(deserializer).map(Self)
    }
}

/// The version of the tool in the answer of `version -o json`: the `azure-cli` member, when it looks like a version
/// (a digit first, then letters, digits and `.`, `-`, `+`, `_`, at most 64 characters). Anything else is no version.
pub fn parse_version(stdout: &str) -> Option<String> {
    if stdout.len() > READ_MAX {
        return None;
    }
    let VersionDoc(raw) = serde_json::from_str(stdout.trim_start_matches('\u{feff}')).ok()?;
    let version = raw.version?;
    let plausible = version.len() <= VERSION_MAX
        && version.starts_with(|c: char| c.is_ascii_digit())
        && version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'));
    plausible.then_some(version)
}

// ---------------------------------------------------------------------------------------------------------------------
// The integration.
// ---------------------------------------------------------------------------------------------------------------------

/// The Azure command-line tool as an [`Integration`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Azure;

/// The one instance.
pub static AZURE: Azure = Azure;

/// `--subscription=<guid>` when the person has selected one.
fn selected(selection: &CheckedSelection, args: &mut Vec<OsString>) {
    if let Some(subscription) = &selection.subscription {
        args.push(flag_eq("subscription", subscription.as_str()));
    }
}

impl Integration for Azure {
    fn spec(&self) -> &'static IntegrationSpec {
        &SPEC
    }

    fn argv(&self, verb: &Verb, args: &Validated, selection: &CheckedSelection) -> Result<Vec<OsString>, ArgError> {
        // Only the verbs of the closed catalogue, by value: a verb of another description with the same id is refused.
        let known = SPEC.verb(verb.id).filter(|known| *known == verb).ok_or(ArgError::Unknown)?;
        let mut argv: Vec<OsString> = Vec::new();
        match known.id {
            "account" => {
                argv.extend(["account", "show"].map(OsString::from));
                argv.extend(JSON_ONLY.map(OsString::from));
                selected(selection, &mut argv);
            }
            "subscriptions" => {
                argv.extend(["account", "list"].map(OsString::from));
                argv.extend(JSON_ONLY.map(OsString::from));
            }
            "groups" => {
                argv.extend(["group", "list"].map(OsString::from));
                argv.extend(JSON_ONLY.map(OsString::from));
                selected(selection, &mut argv);
            }
            "set_subscription" => {
                // The subscription is the validated argument (a GUID), never the selection and never a name.
                let subscription = args.subscription().ok_or(ArgError::Missing)?;
                argv.extend(["account", "set"].map(OsString::from));
                argv.push(flag_eq("subscription", subscription.as_str()));
                argv.push(OsString::from("--only-show-errors"));
            }
            // The sign-in has no flag: no tenant, no device-code switch. The terminal shows what the tool prints.
            "login" => argv.push(OsString::from("login")),
            _ => return Err(ArgError::Unknown),
        }
        Ok(argv)
    }

    fn view(&self, verb: &Verb, stdout: &[u8]) -> Result<ViewModel, ParseError> {
        match verb.id {
            "account" => Ok(account_view(stdout)),
            "subscriptions" => subscriptions_view(stdout),
            "groups" => groups_view(stdout),
            _ => Err(ParseError::Unrecognised),
        }
    }

    fn read_default(&self, verb: &Verb, stdout: &[u8]) -> Option<DefaultRead> {
        if SPEC.verb(verb.id) != Some(verb) || verb.id != "account" {
            return None;
        }
        match login_state(stdout) {
            LoginState::SignedIn { subscription_id, .. } => Some(DefaultRead {
                selection: Selection { subscription: Some(subscription_id.as_str().to_owned()), ..Selection::default() },
                signed_out: false,
                contexts: Vec::new(),
            }),
            LoginState::SignedOut => Some(DefaultRead { signed_out: true, ..DefaultRead::default() }),
            LoginState::Unknown => None,
        }
    }

    fn changes(&self, verb: &Verb) -> Option<&'static str> {
        (SPEC.verb(verb.id) == Some(verb) && verb.id == "set_subscription")
            .then_some("the Azure CLI configuration of this computer (azureProfile.json)")
    }
}
