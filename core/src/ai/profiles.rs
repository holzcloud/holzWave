//! Provider profiles, presets, key accounts and their scope.
//!
//! `ai.json` is the list of the AI providers the user set up: a name, a kind of provider (the preset), the wire it
//! speaks, a base address and a model. It holds no secret of any kind. A key lives in the operating system keychain
//! only, one entry per slot, and the entry holds the key together with the address (scheme, host and port) it was
//! saved for. When the address of a profile no longer matches the address stored with the key, the key is not used:
//! the only way back is to enter it again. The API is write-only: a key goes in as an [`ApiKey`], and everything that
//! leaves is a [`KeyStatus`]; the one function that reads a key back is crate-private and its result goes straight
//! into the header of a request.
//!
//! Nothing in this module opens a socket, looks a name up, starts a thread or fetches a model list. The base address
//! is checked for syntax and for literal addresses only, by the same rule the HTTP chokepoint applies; what is
//! reached, and whether the destination is allowed, is decided when a request is sent.
//!
//! The parts, in order: constants, presets, profiles, the file, the keys, resolution.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use zeroize::Zeroizing;

use super::http::{Http, NetworkPolicy, Origin, Refusal, Rejection, Scheme, SecretHeader, Target};
use super::provider::{AiError, ChatRequest, Detail};
use super::types::{ApiKey, Capabilities, Locality};
use super::wire::{Endpoint, HttpProvider, Wire, WireAdapter, WireRequest, WireTurn};
use crate::layout::LoadStatus;
use crate::persist::{backup_bad, bad_path, read_limited, write_atomic_private};
use crate::secrets::{MAX_LEN, SecretStore};

// ---- constants ------------------------------------------------------------------------------------------

/// The name of the file in the data directory.
pub const FILE: &str = "ai.json";
/// The format number of the file. A file with another number is kept as `ai.json.bad` and not read.
pub const FORMAT_VERSION: u32 = 1;
/// The largest file that is read.
pub const MAX_BYTES: u64 = 262_144;
/// The most profiles a file holds.
pub const MAX_PROFILES: usize = 32;
/// The longest profile id.
pub const MAX_ID: usize = 40;
/// The longest profile name, in characters.
pub const MAX_NAME: usize = 80;
/// The longest model id, in characters.
pub const MAX_MODEL: usize = 200;
/// The most addresses whose gateway disclosure was acknowledged. The oldest go first.
pub const MAX_ACKNOWLEDGED: usize = 64;
/// The longest key, in bytes. With its record it stays under the limit of the secret store.
pub const MAX_KEY_BYTES: usize = 2048;
/// Ids a profile cannot have. `apple` is the on-device provider that the application registers by itself.
pub const RESERVED_IDS: &[&str] = &["apple"];

/// The most notes a load reports; the rest are counted in one last note.
const MAX_NOTES: usize = 32;
/// The format number of the record that holds a key in the keychain.
const RECORD_VERSION: u32 = 1;

// ---- presets --------------------------------------------------------------------------------------------

/// The kind of provider a profile is. The tags are written to `ai.json` and never change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Preset {
    /// Anthropic.
    #[serde(rename = "anthropic")]
    Anthropic,
    /// OpenAI.
    #[serde(rename = "openai")]
    OpenAi,
    /// Google Gemini.
    #[serde(rename = "gemini")]
    Gemini,
    /// Ollama.
    #[serde(rename = "ollama")]
    Ollama,
    /// LM Studio.
    #[serde(rename = "lm_studio")]
    LmStudio,
    /// llama.cpp `llama-server`.
    #[serde(rename = "llama_cpp")]
    LlamaCpp,
    /// OpenRouter.
    #[serde(rename = "openrouter")]
    OpenRouter,
    /// Vercel AI Gateway.
    #[serde(rename = "vercel_gateway")]
    VercelGateway,
    /// Cloudflare AI Gateway.
    #[serde(rename = "cloudflare_gateway")]
    CloudflareGateway,
    /// A LiteLLM proxy.
    #[serde(rename = "litellm")]
    LiteLlm,
    /// Any other server.
    #[serde(rename = "custom")]
    Custom,
}

impl Preset {
    /// Every preset, in the order of [`PRESETS`].
    pub const ALL: [Preset; 11] = [
        Preset::Anthropic,
        Preset::OpenAi,
        Preset::Gemini,
        Preset::Ollama,
        Preset::LmStudio,
        Preset::LlamaCpp,
        Preset::OpenRouter,
        Preset::VercelGateway,
        Preset::CloudflareGateway,
        Preset::LiteLlm,
        Preset::Custom,
    ];

    /// The stable tag.
    pub fn tag(self) -> &'static str {
        match self {
            Preset::Anthropic => "anthropic",
            Preset::OpenAi => "openai",
            Preset::Gemini => "gemini",
            Preset::Ollama => "ollama",
            Preset::LmStudio => "lm_studio",
            Preset::LlamaCpp => "llama_cpp",
            Preset::OpenRouter => "openrouter",
            Preset::VercelGateway => "vercel_gateway",
            Preset::CloudflareGateway => "cloudflare_gateway",
            Preset::LiteLlm => "litellm",
            Preset::Custom => "custom",
        }
    }

    /// The preset with this tag.
    pub fn from_tag(tag: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|preset| preset.tag() == tag)
    }

    /// What is known about this kind of provider.
    pub fn spec(self) -> &'static PresetSpec {
        &PRESETS[self as usize]
    }
}

/// Whether a provider needs a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyNeed {
    /// A key may be saved, and a profile without one works (a local server).
    Optional,
    /// Nothing is sent without a key.
    Required,
}

/// Who sees the whole prompt besides the model provider, and so asks for an acknowledgement before the first send.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisclosureKind {
    /// A gateway or router that forwards the request to a model provider and may log it.
    Gateway,
    /// A server of a third party that is not one of the named providers.
    ThirdParty,
}

/// What is known about one kind of provider: the wires it speaks, the first of which is the default, the default
/// address of each wire, whether it needs a key, whether it takes a second token for a gateway in front of it, whether
/// a third party sees the prompt, and a part of the request body that the preset adds to whatever the adapter wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresetSpec {
    /// The preset this row describes.
    pub preset: Preset,
    /// The name shown to the user.
    pub label: &'static str,
    /// The wires the provider speaks. The first is the default.
    pub wires: &'static [Wire],
    /// The default base address of a wire. An address with a part in braces still has to be filled in; a wire with no
    /// row here has no default.
    pub bases: &'static [(Wire, &'static str)],
    /// Whether a key is needed.
    pub key: KeyNeed,
    /// Whether the provider takes a second token, for a gateway in front of it.
    pub gateway_token: bool,
    /// The acknowledgement a non-local address asks for, if any.
    pub disclosure: Option<DisclosureKind>,
    /// A JSON object merged into the body of every request, with its values winning over the adapter's. Empty for none.
    pub extra_body: &'static str,
}

// presets-begin
/// The presets, in the order of [`Preset::ALL`]. The default addresses are data: the application contacts none of them
/// by itself, only the HTTP chokepoint does, and only for a request the user sent.
pub static PRESETS: [PresetSpec; 11] = [
    PresetSpec {
        preset: Preset::Anthropic,
        label: "Anthropic",
        wires: &[Wire::Anthropic],
        bases: &[(Wire::Anthropic, "https://api.anthropic.com")],
        key: KeyNeed::Required,
        gateway_token: false,
        disclosure: None,
        extra_body: "",
    },
    PresetSpec {
        preset: Preset::OpenAi,
        label: "OpenAI",
        wires: &[Wire::OpenAiResponses],
        bases: &[(Wire::OpenAiResponses, "https://api.openai.com/v1")],
        key: KeyNeed::Required,
        gateway_token: false,
        disclosure: None,
        extra_body: "",
    },
    PresetSpec {
        preset: Preset::Gemini,
        label: "Google Gemini",
        wires: &[Wire::Gemini],
        bases: &[(Wire::Gemini, "https://generativelanguage.googleapis.com/v1beta")],
        key: KeyNeed::Required,
        gateway_token: false,
        disclosure: None,
        extra_body: "",
    },
    PresetSpec {
        preset: Preset::Ollama,
        label: "Ollama",
        wires: &[Wire::Ollama],
        bases: &[(Wire::Ollama, "http://127.0.0.1:11434")],
        key: KeyNeed::Optional,
        gateway_token: false,
        disclosure: None,
        extra_body: "",
    },
    PresetSpec {
        preset: Preset::LmStudio,
        label: "LM Studio",
        wires: &[Wire::OpenAiChat],
        bases: &[(Wire::OpenAiChat, "http://127.0.0.1:1234/v1")],
        key: KeyNeed::Optional,
        gateway_token: false,
        disclosure: None,
        extra_body: "",
    },
    PresetSpec {
        preset: Preset::LlamaCpp,
        label: "llama.cpp",
        wires: &[Wire::OpenAiChat],
        bases: &[(Wire::OpenAiChat, "http://127.0.0.1:9931/v1")],
        key: KeyNeed::Optional,
        gateway_token: false,
        disclosure: None,
        extra_body: "",
    },
    PresetSpec {
        preset: Preset::OpenRouter,
        label: "OpenRouter",
        wires: &[Wire::OpenAiChat],
        bases: &[(Wire::OpenAiChat, "https://openrouter.ai/api/v1")],
        key: KeyNeed::Required,
        gateway_token: false,
        disclosure: Some(DisclosureKind::Gateway),
        extra_body: r#"{"provider":{"data_collection":"deny"}}"#,
    },
    PresetSpec {
        preset: Preset::VercelGateway,
        label: "Vercel AI Gateway",
        wires: &[Wire::OpenAiChat, Wire::Anthropic],
        bases: &[
            (Wire::OpenAiChat, "https://ai-gateway.vercel.sh/v1"),
            (Wire::Anthropic, "https://ai-gateway.vercel.sh"),
        ],
        key: KeyNeed::Required,
        gateway_token: false,
        disclosure: Some(DisclosureKind::Gateway),
        extra_body: "",
    },
    PresetSpec {
        preset: Preset::CloudflareGateway,
        label: "Cloudflare AI Gateway",
        wires: &[Wire::Anthropic, Wire::OpenAiResponses, Wire::OpenAiChat, Wire::Gemini],
        bases: &[
            (Wire::Anthropic, "https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway_id}/anthropic"),
            (Wire::OpenAiResponses, "https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway_id}/openai"),
            (Wire::OpenAiChat, "https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway_id}/openai"),
            (Wire::Gemini, "https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway_id}/google-ai-studio/v1beta"),
        ],
        key: KeyNeed::Optional,
        gateway_token: true,
        disclosure: Some(DisclosureKind::Gateway),
        extra_body: "",
    },
    PresetSpec {
        preset: Preset::LiteLlm,
        label: "LiteLLM",
        wires: &[Wire::OpenAiChat],
        bases: &[(Wire::OpenAiChat, "http://127.0.0.1:4000/v1")],
        key: KeyNeed::Optional,
        gateway_token: false,
        disclosure: Some(DisclosureKind::Gateway),
        extra_body: "",
    },
    PresetSpec {
        preset: Preset::Custom,
        label: "Custom",
        wires: &[Wire::OpenAiChat, Wire::OpenAiResponses, Wire::Anthropic, Wire::Gemini, Wire::Ollama],
        bases: &[],
        key: KeyNeed::Optional,
        gateway_token: false,
        disclosure: Some(DisclosureKind::ThirdParty),
        extra_body: "",
    },
];
// presets-end

impl PresetSpec {
    /// The default base address of a wire, if the preset has one.
    pub fn base_for(&self, wire: Wire) -> Option<&'static str> {
        self.bases.iter().find(|(known, _)| *known == wire).map(|(_, base)| *base)
    }
}

/// The header that carries a key for a wire, and the text that goes before the key: the name and the prefix.
pub fn auth_header_for(wire: Wire) -> (&'static str, &'static str) {
    match wire {
        Wire::Anthropic => ("x-api-key", ""),
        Wire::Gemini => ("x-goog-api-key", ""),
        Wire::OpenAiChat | Wire::OpenAiResponses | Wire::Ollama => ("authorization", "Bearer "),
    }
}

// ---- profiles -------------------------------------------------------------------------------------------

/// One provider the user set up. It holds no secret: `key_set` and `gateway_key_set` only say that an entry was saved
/// in the keychain, so that a profile without a key never touches the keychain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// The stable id: 1 to 40 characters of `a-z`, `0-9`, `-` and `_`.
    pub id: String,
    /// The name the user gave it.
    pub name: String,
    /// The kind of provider.
    pub preset: Preset,
    /// The wire it is spoken on.
    pub wire: Wire,
    /// The base address.
    pub base_url: String,
    /// The model, empty until the user chose one.
    pub model: String,
    /// Plain http to the local network is allowed for this profile.
    pub allow_lan_http: bool,
    /// A key was saved for the provider slot.
    #[serde(default)]
    pub key_set: bool,
    /// A key was saved for the gateway slot.
    #[serde(default)]
    pub gateway_key_set: bool,
}

/// Where a profile points, for display and for the comparison with the address a key was saved for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    /// Scheme, host and port: the origin scope a key is filed for.
    pub scope: String,
    /// The host as it is shown: with its port when the port is not the default of the scheme, an IPv6 host in
    /// brackets.
    pub host: String,
}

impl Profile {
    /// The address the profile points at, or the rule it breaks. Nothing is looked up.
    pub fn address(&self) -> Result<Address, Refusal> {
        let target = Target::parse(&self.base_url, self.allow_lan_http)?;
        Ok(address_of(&target))
    }

    /// The parsed base address, with the same rule at the settings boundary, at load and at every resolution.
    fn target(&self) -> Result<Target, ProfileError> {
        parse_address(&self.base_url, self.allow_lan_http)
    }
}

fn address_of(target: &Target) -> Address {
    let origin = target.origin();
    let host = if origin.host().contains(':') { format!("[{}]", origin.host()) } else { origin.host().to_owned() };
    let default_port = match origin.scheme() {
        Scheme::Https => 443,
        Scheme::Http => 80,
    };
    let host = if origin.port() == default_port { host } else { format!("{host}:{}", origin.port()) };
    Address { scope: origin.scope(), host }
}

/// What the user typed to make or change a profile. `Debug` prints the preset, the wire, the flags and sizes, never the
/// name, the address or the model: an address that was typed with a key in its query is refused, but the text was typed
/// and must not be printed on the way to the refusal.
#[derive(Clone, PartialEq, Eq)]
pub struct ProfileInput {
    /// The id of the profile to change; `None` makes a new profile with an id made from the name.
    pub id: Option<String>,
    /// The name.
    pub name: String,
    /// The kind of provider. It cannot change for an existing profile.
    pub preset: Preset,
    /// The wire; `None` keeps the wire of an existing profile and takes the default of the preset for a new one.
    pub wire: Option<Wire>,
    /// The base address.
    pub base_url: String,
    /// The model.
    pub model: String,
    /// Plain http to the local network is allowed.
    pub allow_lan_http: bool,
    /// The user has read the disclosure for this address and accepts it: the origin is recorded as acknowledged when
    /// the preset asks for an acknowledgement there.
    pub acknowledge_gateway: bool,
}

impl fmt::Debug for ProfileInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProfileInput")
            .field("preset", &self.preset)
            .field("wire", &self.wire)
            .field("has_id", &self.id.is_some())
            .field("name_bytes", &self.name.len())
            .field("base_url_bytes", &self.base_url.len())
            .field("model_bytes", &self.model.len())
            .field("allow_lan_http", &self.allow_lan_http)
            .field("acknowledge_gateway", &self.acknowledge_gateway)
            .finish()
    }
}

impl ProfileInput {
    /// A new profile with everything else at its empty default.
    pub fn new(name: impl Into<String>, preset: Preset) -> Self {
        Self {
            id: None,
            name: name.into(),
            preset,
            wire: None,
            base_url: String::new(),
            model: String::new(),
            allow_lan_http: false,
            acknowledge_gateway: false,
        }
    }
}

/// Why a profile was refused. The text is a fixed phrase and never holds the input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileError {
    /// The id is not 1 to 40 characters of `a-z`, `0-9`, `-` and `_`.
    Id,
    /// The id is one the application keeps for itself.
    ReservedId,
    /// The name is empty, too long or holds a control character.
    Name,
    /// The model is too long or holds a control character.
    Model,
    /// The preset does not speak this wire.
    Wire,
    /// The preset of an existing profile cannot change.
    PresetChange,
    /// The address is empty or still holds a part to fill in.
    NeedsAddress,
    /// The address breaks a rule of the chokepoint.
    Address(Refusal),
    /// There are already [`MAX_PROFILES`] profiles.
    TooMany,
    /// There is no profile with this id.
    UnknownProfile,
}

impl ProfileError {
    /// The stable lower-case name the settings dialog keys its text on. An address refusal gives the name of the
    /// [`Refusal`].
    pub fn reason(self) -> &'static str {
        match self {
            ProfileError::Id => "id",
            ProfileError::ReservedId => "reserved_id",
            ProfileError::Name => "name",
            ProfileError::Model => "model",
            ProfileError::Wire => "wire",
            ProfileError::PresetChange => "preset_change",
            ProfileError::NeedsAddress => "needs_address",
            ProfileError::Address(refusal) => refusal.as_str(),
            ProfileError::TooMany => "too_many",
            ProfileError::UnknownProfile => "unknown_profile",
        }
    }
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileError::Id => write!(f, "the id must be 1 to {MAX_ID} characters of a to z, 0 to 9, - and _"),
            ProfileError::ReservedId => f.write_str("this id is reserved"),
            ProfileError::Name => write!(f, "the name must be 1 to {MAX_NAME} characters without control characters"),
            ProfileError::Model => {
                write!(f, "the model must be at most {MAX_MODEL} characters without control characters")
            }
            ProfileError::Wire => f.write_str("this kind of provider does not speak that wire"),
            ProfileError::PresetChange => f.write_str("the kind of provider of a profile cannot change"),
            ProfileError::NeedsAddress => f.write_str("the address is empty or still has a part to fill in"),
            ProfileError::Address(refusal) => fmt::Display::fmt(refusal, f),
            ProfileError::TooMany => write!(f, "there are already {MAX_PROFILES} profiles"),
            ProfileError::UnknownProfile => f.write_str("there is no profile with this id"),
        }
    }
}

impl std::error::Error for ProfileError {}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().count() <= MAX_ID && id.chars().all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-' | '_'))
}

fn is_reserved(id: &str) -> bool {
    RESERVED_IDS.contains(&id)
}

/// At most `max` characters and no control character.
fn clean_text(text: &str, max: usize) -> bool {
    text.chars().count() <= max && !text.chars().any(char::is_control)
}

/// Whether the address still holds a part to fill in: a brace, or a percent-encoded brace in any case.
fn has_template(text: &str) -> bool {
    if text.contains(['{', '}']) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    lower.contains("%7b") || lower.contains("%7d")
}

/// Whether the host is this computer: a loopback address literal, `localhost` or a name below it.
fn is_loopback_host(target: &Target) -> bool {
    let host = target.origin().host();
    match target.literal_locality() {
        Some(locality) => locality == Locality::Loopback,
        None => host == "localhost" || host.ends_with(".localhost"),
    }
}

/// The acknowledgement a preset asks for at this address: none for a provider that is named in the table, and none for
/// any preset when the host is this computer.
fn disclosure_for(preset: Preset, target: &Target) -> Option<DisclosureKind> {
    preset.spec().disclosure.filter(|_| !is_loopback_host(target))
}

/// The one address rule: the template check, then the syntax and literal-address rules of the chokepoint.
fn parse_address(raw: &str, allow_lan_http: bool) -> Result<Target, ProfileError> {
    let raw = raw.trim_matches(|c: char| c.is_ascii_whitespace());
    if raw.is_empty() || has_template(raw) {
        return Err(ProfileError::NeedsAddress);
    }
    Target::parse(raw, allow_lan_http).map_err(ProfileError::Address)
}

/// The checks of a profile that was read from a file.
fn check_stored(profile: &Profile) -> Result<(), ProfileError> {
    if !valid_id(&profile.id) {
        return Err(ProfileError::Id);
    }
    if is_reserved(&profile.id) {
        return Err(ProfileError::ReservedId);
    }
    if profile.name.is_empty() || !clean_text(&profile.name, MAX_NAME) {
        return Err(ProfileError::Name);
    }
    if !clean_text(&profile.model, MAX_MODEL) {
        return Err(ProfileError::Model);
    }
    if !profile.preset.spec().wires.contains(&profile.wire) {
        return Err(ProfileError::Wire);
    }
    profile.target().map(|_| ())
}

// ---- the keys: slots, status, errors --------------------------------------------------------------------

/// Which key of a profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySlot {
    /// The key of the provider.
    Provider,
    /// The token of a gateway in front of the provider.
    Gateway,
}

/// The keychain account of a slot: `ai:<id>` for the provider key, `ai:<id>:gateway` for a gateway token.
pub fn account(profile_id: &str, slot: KeySlot) -> String {
    match slot {
        KeySlot::Provider => format!("ai:{profile_id}"),
        KeySlot::Gateway => format!("ai:{profile_id}:gateway"),
    }
}

/// Whether a profile of this preset has this slot.
fn slot_exists(preset: Preset, slot: KeySlot) -> bool {
    slot == KeySlot::Provider || preset.spec().gateway_token
}

fn marker(profile: &Profile, slot: KeySlot) -> bool {
    match slot {
        KeySlot::Provider => profile.key_set,
        KeySlot::Gateway => profile.gateway_key_set,
    }
}

fn set_marker(profile: &mut Profile, slot: KeySlot, value: bool) {
    match slot {
        KeySlot::Provider => profile.key_set = value,
        KeySlot::Gateway => profile.gateway_key_set = value,
    }
}

/// What can be said about a key without showing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    /// No key was saved.
    NotSet,
    /// A key is saved for the address of the profile.
    Present,
    /// The profile says a key was saved and the keychain has none.
    Missing,
    /// A key is saved for another address and is not used.
    WrongAddress,
    /// The entry in the keychain is not a record of this version.
    Damaged,
    /// The keychain answered with an error.
    Unreadable,
    /// There is no system keychain, so no key can be saved or used.
    NoSecureStore,
}

impl KeyStatus {
    /// The stable name.
    pub fn as_str(self) -> &'static str {
        match self {
            KeyStatus::NotSet => "not_set",
            KeyStatus::Present => "present",
            KeyStatus::Missing => "missing",
            KeyStatus::WrongAddress => "wrong_address",
            KeyStatus::Damaged => "damaged",
            KeyStatus::Unreadable => "unreadable",
            KeyStatus::NoSecureStore => "no_secure_store",
        }
    }
}

/// Why a key could not be saved or cleared. The text is a fixed phrase: neither the key nor the words of the keychain
/// library are in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyError {
    /// There is no system keychain, and a key is never kept anywhere else.
    NoSecureStore,
    /// The keychain answered with an error.
    Store,
    /// The key cannot be part of a header, or is too long.
    Invalid(&'static str),
    /// There is no profile with this id.
    UnknownProfile,
    /// The profile has no such slot.
    NoSuchSlot,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::NoSecureStore => f.write_str("there is no system keychain, so a key cannot be saved"),
            KeyError::Store => f.write_str("the system keychain refused the request"),
            KeyError::Invalid(why) => f.write_str(why),
            KeyError::UnknownProfile => f.write_str("there is no profile with this id"),
            KeyError::NoSuchSlot => f.write_str("this kind of provider has no such key"),
        }
    }
}

impl std::error::Error for KeyError {}

/// Why a stored key could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    NoSecureStore,
    Store,
    Damaged,
}

/// A key read back from the keychain with the address it was saved for. It lives long enough to become a header.
pub(crate) struct Stored {
    origin: Origin,
    key: ApiKey,
}

/// The record in the keychain: one JSON object with the format number, the origin scope and the key.
#[derive(Serialize)]
struct RecordOut<'a> {
    v: u32,
    scope: &'a str,
    key: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordIn {
    v: u32,
    scope: String,
    key: String,
}

/// The system keychain, seen through the one door that keys go through.
///
/// A key is written with [`AiConfig::set_key`] and never comes back out: the only read is crate-private. Nothing here
/// writes a key to a file or keeps one in memory when there is no keychain.
///
/// The read path cannot be called from outside the crate:
///
/// ```compile_fail
/// use holzwave_core::ai::profiles::KeyVault;
/// let vault = KeyVault::unavailable();
/// let _ = vault.load("ai:x");
/// ```
///
/// The failing example above stands beside this one, which uses the same import and constructor, so a typo cannot
/// make it pass:
///
/// ```
/// use holzwave_core::ai::profiles::KeyVault;
/// let vault = KeyVault::unavailable();
/// assert!(!vault.is_available());
/// ```
#[derive(Clone)]
pub struct KeyVault {
    store: Option<Arc<dyn SecretStore>>,
}

impl KeyVault {
    /// A vault over a secret store. Nothing is read or written.
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self { store: Some(store) }
    }

    /// A vault without a store: a key cannot be saved, and a profile that needs one is unavailable. This is what a
    /// system without a keychain gets.
    pub fn unavailable() -> Self {
        Self { store: None }
    }

    /// Whether there is a store.
    pub fn is_available(&self) -> bool {
        self.store.is_some()
    }

    /// What can be said about the key of a slot of `profile`. It reads the keychain only when the profile says a key
    /// was saved, because the keychain has no way to ask whether an entry exists. It never returns key text.
    pub fn status(&self, profile: &Profile, slot: KeySlot) -> KeyStatus {
        if !marker(profile, slot) {
            return KeyStatus::NotSet;
        }
        match self.load(&account(&profile.id, slot)) {
            Err(Fault::NoSecureStore) => KeyStatus::NoSecureStore,
            Err(Fault::Store) => KeyStatus::Unreadable,
            Err(Fault::Damaged) => KeyStatus::Damaged,
            Ok(None) => KeyStatus::Missing,
            Ok(Some(stored)) => match profile.target() {
                Ok(target) if *target.origin() == stored.origin => KeyStatus::Present,
                _ => KeyStatus::WrongAddress,
            },
        }
    }

    /// The one read of a stored key: the record of `account`, if there is one. The text the keychain returned is
    /// wiped when this function ends, and the key is wrapped at once.
    pub(crate) fn load(&self, account: &str) -> Result<Option<Stored>, Fault> {
        let store = self.store.as_ref().ok_or(Fault::NoSecureStore)?;
        let Some(text) = store.get(account).map_err(|_| Fault::Store)? else { return Ok(None) };
        let text = Zeroizing::new(text);
        let record: RecordIn = serde_json::from_str(&text).map_err(|_| Fault::Damaged)?;
        let key = ApiKey::new(record.key);
        if record.v != RECORD_VERSION || key.is_empty() {
            return Err(Fault::Damaged);
        }
        let origin = Origin::from_scope(&record.scope).map_err(|_| Fault::Damaged)?;
        Ok(Some(Stored { origin, key }))
    }

    /// Writes the record of a key.
    fn save(&self, account: &str, origin: &Origin, key: &str) -> Result<(), KeyError> {
        let store = self.store.as_ref().ok_or(KeyError::NoSecureStore)?;
        let scope = origin.scope();
        let text = serde_json::to_string(&RecordOut { v: RECORD_VERSION, scope: &scope, key })
            .map_err(|_| KeyError::Invalid("the key cannot be stored"))?;
        let text = Zeroizing::new(text);
        if text.len() > MAX_LEN {
            return Err(KeyError::Invalid("the key is too long to store"));
        }
        store.set(account, &text).map_err(|_| KeyError::Store)
    }

    /// Removes the record of an account. Removing one that does not exist is not an error.
    fn delete(&self, account: &str) -> Result<(), KeyError> {
        let store = self.store.as_ref().ok_or(KeyError::NoSecureStore)?;
        store.delete(account).map_err(|_| KeyError::Store)
    }
}

impl fmt::Debug for KeyVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyVault").field("available", &self.is_available()).finish()
    }
}

/// The key as it is stored: trimmed, and only characters that can be part of a header value.
fn clean_key(key: &ApiKey) -> Result<&str, KeyError> {
    let text = key.expose().trim_matches(|c: char| c.is_ascii_whitespace());
    if text.is_empty() {
        return Err(KeyError::Invalid("the key is empty"));
    }
    if text.len() > MAX_KEY_BYTES {
        return Err(KeyError::Invalid("the key is too long"));
    }
    if !text.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(KeyError::Invalid("the key has a character that cannot be part of a header"));
    }
    Ok(text)
}

// ---- the file -------------------------------------------------------------------------------------------

/// The notes of a load, bounded: a hostile file with thousands of bad entries costs a few lines, not a few megabytes.
#[derive(Default)]
struct Notes {
    lines: Vec<String>,
    left_out: usize,
}

impl Notes {
    fn add(&mut self, line: String) {
        if self.lines.len() < MAX_NOTES {
            self.lines.push(line);
        } else {
            self.left_out += 1;
        }
    }

    fn finish(mut self) -> Vec<String> {
        if self.left_out > 0 {
            self.lines.push(format!("{} more notes left out", self.left_out));
        }
        self.lines
    }
}

/// The document as it is written: a version, the profiles and the acknowledged addresses.
#[derive(Serialize)]
struct FileOut<'a> {
    version: u32,
    profiles: &'a [Profile],
    acknowledged: &'a [String],
}

/// The profiles and the addresses whose disclosure was acknowledged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AiConfig {
    profiles: Vec<Profile>,
    acknowledged: Vec<String>,
}

/// What removing a profile did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Removal {
    /// There was such a profile and it is gone.
    pub removed: bool,
    /// No key of the profile is left in the keychain.
    pub key_deleted: bool,
}

impl AiConfig {
    /// The profiles, in file order.
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// The profile with this id.
    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    /// Whether the disclosure of this origin scope was acknowledged.
    pub fn is_acknowledged(&self, scope: &str) -> bool {
        self.acknowledged.iter().any(|known| known == scope)
    }

    /// Reads a document. It never fails and never panics: a text that cannot be used gives an empty configuration and
    /// `Reset` with a reason that quotes no content; a profile that breaks a rule is dropped with a note and costs only
    /// itself.
    pub fn from_json(text: &str) -> (AiConfig, LoadStatus) {
        let reset = |reason: String| (AiConfig::default(), LoadStatus::Reset(reason));
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return reset("the file is not valid JSON".to_owned());
        };
        let Value::Object(mut root) = value else {
            return reset("the file is not a JSON object".to_owned());
        };
        match root.get("version").and_then(Value::as_u64) {
            None => return reset("the file has no usable version number".to_owned()),
            Some(version) if version != u64::from(FORMAT_VERSION) => {
                return reset(format!("the file has version {version}, this build reads version {FORMAT_VERSION}"));
            }
            Some(_) => {}
        }
        let mut notes = Notes::default();
        let mut config = AiConfig::default();
        let list = match root.remove("profiles") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(list)) => list,
            Some(_) => {
                notes.add("the list of profiles is not a list".to_owned());
                Vec::new()
            }
        };
        for (index, value) in list.into_iter().enumerate() {
            let number = index + 1;
            let Ok(profile) = Profile::deserialize(value) else {
                notes.add(format!("profile #{number}: not a profile of this build"));
                continue;
            };
            if let Err(reason) = config.admit(profile) {
                notes.add(format!("profile #{number}: {reason}"));
            }
        }
        match root.remove("acknowledged") {
            None | Some(Value::Null) => {}
            Some(Value::Array(list)) => {
                for (index, value) in list.into_iter().enumerate() {
                    let origin = value.as_str().and_then(|scope| Origin::from_scope(scope).ok());
                    match origin {
                        Some(origin) => config.remember(&origin.scope()),
                        None => notes.add(format!("acknowledged address #{}: not an address scope", index + 1)),
                    }
                }
            }
            Some(_) => notes.add("the list of acknowledged addresses is not a list".to_owned()),
        }
        let notes = notes.finish();
        if notes.is_empty() { (config, LoadStatus::Clean) } else { (config, LoadStatus::Repaired(notes)) }
    }

    /// Keeps a profile that was read from a file, or says why not.
    fn admit(&mut self, profile: Profile) -> Result<(), String> {
        check_stored(&profile).map_err(|error| error.to_string())?;
        if self.get(&profile.id).is_some() {
            return Err("an earlier profile has the same id".to_owned());
        }
        if self.profiles.len() >= MAX_PROFILES {
            return Err(ProfileError::TooMany.to_string());
        }
        self.profiles.push(profile);
        Ok(())
    }

    /// Records an origin scope as acknowledged: no duplicate, at most [`MAX_ACKNOWLEDGED`], the oldest dropped.
    fn remember(&mut self, scope: &str) {
        self.acknowledged.retain(|known| known != scope);
        self.acknowledged.push(scope.to_owned());
        if self.acknowledged.len() > MAX_ACKNOWLEDGED {
            let excess = self.acknowledged.len() - MAX_ACKNOWLEDGED;
            self.acknowledged.drain(..excess);
        }
    }

    /// The document as pretty printed JSON with two-space indentation and one trailing newline.
    pub fn to_json(&self) -> String {
        let document = FileOut { version: FORMAT_VERSION, profiles: &self.profiles, acknowledged: &self.acknowledged };
        // Strings, booleans and lists of them cannot fail to serialise; the fallback only keeps the function total.
        let mut text = serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_owned());
        text.push('\n');
        text
    }

    /// Reads `<dir>/ai.json`.
    ///
    /// A missing file is empty and `Clean`. A file that cannot be used is empty and `Reset`, and its bytes are kept
    /// as `ai.json.bad` first so that the next save does not destroy them; so is a file that had to be repaired. A
    /// file over [`MAX_BYTES`] is not read: it is moved aside. The reasons name limits and version numbers and quote
    /// nothing from the file.
    pub fn load(dir: &Path) -> (AiConfig, LoadStatus) {
        let path = dir.join(FILE);
        let bytes = match read_limited(&path, MAX_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return (AiConfig::default(), LoadStatus::Clean),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                let kept = fs::rename(&path, bad_path(&path)).is_ok();
                let tail = if kept { "; the file was moved to ai.json.bad" } else { "; it could not be kept" };
                let reason = format!("the file is larger than {MAX_BYTES} bytes{tail}");
                return (AiConfig::default(), LoadStatus::Reset(reason));
            }
            Err(_) => return (AiConfig::default(), LoadStatus::Reset("the file could not be read".to_owned())),
        };
        let (config, status) = match String::from_utf8(bytes) {
            Ok(text) => AiConfig::from_json(&text),
            Err(_) => (AiConfig::default(), LoadStatus::Reset("the file is not UTF-8 text".to_owned())),
        };
        match status {
            LoadStatus::Reset(reason) => {
                let reason = match backup_bad(&path) {
                    Ok(_) => reason,
                    Err(_) => format!("{reason}; the damaged file could not be kept"),
                };
                (config, LoadStatus::Reset(reason))
            }
            LoadStatus::Repaired(mut notes) => {
                if backup_bad(&path).is_err() {
                    notes.push("the file as it was could not be kept".to_owned());
                }
                (config, LoadStatus::Repaired(notes))
            }
            LoadStatus::Clean => (config, LoadStatus::Clean),
        }
    }

    /// Writes `<dir>/ai.json` atomically, readable by the user only.
    pub fn save(&self, dir: &Path) -> io::Result<()> {
        write_atomic_private(&dir.join(FILE), self.to_json().as_bytes())
    }

    /// Makes a profile or changes one, and returns its id.
    ///
    /// The rules, in this order: the id, a change of preset, the wire, the name, the model, the address, the count.
    /// The address is checked by the rule the chokepoint applies. The key markers of an existing profile are kept.
    pub fn upsert(&mut self, input: ProfileInput) -> Result<String, ProfileError> {
        let existing = match &input.id {
            Some(id) => {
                if !valid_id(id) {
                    return Err(ProfileError::Id);
                }
                if is_reserved(id) {
                    return Err(ProfileError::ReservedId);
                }
                Some(self.profiles.iter().position(|profile| profile.id == *id).ok_or(ProfileError::UnknownProfile)?)
            }
            None => None,
        };
        let spec = input.preset.spec();
        if existing.is_some_and(|index| self.profiles[index].preset != input.preset) {
            return Err(ProfileError::PresetChange);
        }
        let wire = input
            .wire
            .or_else(|| existing.map(|index| self.profiles[index].wire))
            .or_else(|| spec.wires.first().copied())
            .ok_or(ProfileError::Wire)?;
        if !spec.wires.contains(&wire) {
            return Err(ProfileError::Wire);
        }
        let name = input.name.trim();
        if name.is_empty() || !clean_text(name, MAX_NAME) {
            return Err(ProfileError::Name);
        }
        let model = input.model.trim();
        if !clean_text(model, MAX_MODEL) {
            return Err(ProfileError::Model);
        }
        // An empty address takes the default of the wire; a default that still has a part to fill in is refused like
        // an empty address.
        let typed = input.base_url.trim_matches(|c: char| c.is_ascii_whitespace());
        let base_url = if typed.is_empty() { spec.base_for(wire).unwrap_or("") } else { typed };
        let target = parse_address(base_url, input.allow_lan_http)?;
        if existing.is_none() && self.profiles.len() >= MAX_PROFILES {
            return Err(ProfileError::TooMany);
        }
        let profile = Profile {
            id: String::new(),
            name: name.to_owned(),
            preset: input.preset,
            wire,
            base_url: base_url.to_owned(),
            model: model.to_owned(),
            allow_lan_http: input.allow_lan_http,
            key_set: existing.is_some_and(|index| self.profiles[index].key_set),
            gateway_key_set: existing.is_some_and(|index| self.profiles[index].gateway_key_set),
        };
        let id = match existing {
            Some(index) => {
                let id = self.profiles[index].id.clone();
                self.profiles[index] = Profile { id: id.clone(), ..profile };
                id
            }
            None => {
                let id = self.fresh_id(name);
                self.profiles.push(Profile { id: id.clone(), ..profile });
                id
            }
        };
        if input.acknowledge_gateway && disclosure_for(input.preset, &target).is_some() {
            self.remember(&target.origin().scope());
        }
        Ok(id)
    }

    /// An id for a new profile made from `name`: lower case letters and digits with `-` for anything else, cut so
    /// that a suffix fits, `provider` when nothing is left, and `-2`, `-3` while it is taken or reserved.
    fn fresh_id(&self, name: &str) -> String {
        let mut base = String::new();
        for c in name.chars().flat_map(char::to_lowercase) {
            let c = if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' };
            if c == '-' && base.ends_with('-') {
                continue;
            }
            base.push(c);
        }
        let base: String = base.trim_matches('-').chars().take(MAX_ID - 4).collect();
        let base = base.trim_matches('-');
        let base = if base.is_empty() { "provider" } else { base };
        let mut id = base.to_owned();
        let mut number = 2;
        while self.get(&id).is_some() || is_reserved(&id) {
            id = format!("{base}-{number}");
            number += 1;
        }
        id
    }

    /// Removes a profile, then the keychain entries of the slots that had a key. A keychain that fails does not keep
    /// the profile: `key_deleted` says whether no key is left.
    pub fn remove(&mut self, id: &str, vault: &KeyVault) -> Removal {
        let Some(index) = self.profiles.iter().position(|profile| profile.id == id) else {
            return Removal { removed: false, key_deleted: false };
        };
        let profile = self.profiles.remove(index);
        let mut key_deleted = true;
        for slot in [KeySlot::Provider, KeySlot::Gateway] {
            if marker(&profile, slot) && vault.delete(&account(&profile.id, slot)).is_err() {
                key_deleted = false;
            }
        }
        Removal { removed: true, key_deleted }
    }

    /// Saves a key for a slot of a profile, filed for the address the profile has now. The key is written first and
    /// the marker second, so a failure leaves an unused key and never a used key without its address.
    pub fn set_key(&mut self, vault: &KeyVault, id: &str, slot: KeySlot, key: ApiKey) -> Result<(), KeyError> {
        let index = self.profiles.iter().position(|profile| profile.id == id).ok_or(KeyError::UnknownProfile)?;
        if !slot_exists(self.profiles[index].preset, slot) {
            return Err(KeyError::NoSuchSlot);
        }
        if !vault.is_available() {
            return Err(KeyError::NoSecureStore);
        }
        let text = clean_key(&key)?;
        let target = self.profiles[index]
            .target()
            .map_err(|_| KeyError::Invalid("the address of the profile cannot be used"))?;
        vault.save(&account(id, slot), target.origin(), text)?;
        set_marker(&mut self.profiles[index], slot, true);
        Ok(())
    }

    /// Removes the key of a slot. The marker is cleared first and the entry second, so a failure leaves an unused
    /// key and never a marker for a key that is gone.
    pub fn clear_key(&mut self, vault: &KeyVault, id: &str, slot: KeySlot) -> Result<(), KeyError> {
        let index = self.profiles.iter().position(|profile| profile.id == id).ok_or(KeyError::UnknownProfile)?;
        if !slot_exists(self.profiles[index].preset, slot) {
            return Err(KeyError::NoSuchSlot);
        }
        if !vault.is_available() {
            // Nothing can be deleted, so the marker keeps telling the truth.
            return if marker(&self.profiles[index], slot) { Err(KeyError::NoSecureStore) } else { Ok(()) };
        }
        set_marker(&mut self.profiles[index], slot, false);
        vault.delete(&account(id, slot))
    }
}

// ---- resolution -----------------------------------------------------------------------------------------

/// A profile and its key, made into the endpoint the chokepoint sends to. It holds a key, so it is not `Clone`, and its
/// `Debug` prints labels only.
pub struct Resolved {
    profile_id: String,
    wire: Wire,
    model: String,
    preset: Preset,
    address: Address,
    endpoint: Endpoint,
}

impl Resolved {
    /// The id of the profile.
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    /// The wire to build an adapter for.
    pub fn wire(&self) -> Wire {
        self.wire
    }

    /// The model the profile names.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The kind of provider.
    pub fn preset(&self) -> Preset {
        self.preset
    }

    /// The origin scope requests go to: scheme, host and port.
    pub fn scope(&self) -> &str {
        &self.address.scope
    }

    /// The host as it is shown to the user.
    pub fn host_label(&self) -> &str {
        &self.address.host
    }

    /// The provider that sends through the chokepoint, with the adapter of the wire. When the preset adds a part to
    /// every request body (OpenRouter is asked not to let providers collect the data), the adapter is wrapped so that
    /// the preset's values win over the adapter's.
    pub fn into_provider(self, adapter: Box<dyn WireAdapter>, http: Arc<Http>) -> HttpProvider {
        let extra = self.preset.spec().extra_body;
        let adapter: Box<dyn WireAdapter> =
            if extra.is_empty() { adapter } else { Box::new(WithExtraBody::new(adapter, extra)) };
        HttpProvider::new(self.profile_id, self.endpoint, adapter, http)
    }
}

impl fmt::Debug for Resolved {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolved")
            .field("profile", &self.profile_id)
            .field("preset", &self.preset)
            .field("wire", &self.wire)
            .field("scope", &self.address.scope)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

/// Why a profile cannot be used to send a request right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// There is no profile with this id.
    UnknownProfile,
    /// The address breaks a rule of the chokepoint.
    Address(Refusal),
    /// The address is empty or still has a part to fill in.
    NeedsAddress,
    /// The address must be acknowledged once before the first send.
    DisclosureNeeded,
    /// The profile needs a key for this slot and has none.
    KeyMissing(KeySlot),
    /// The key was saved for another address.
    KeyScope(KeySlot),
    /// The record of the key is damaged.
    KeyDamaged(KeySlot),
    /// There is no system keychain.
    NoSecureStore,
    /// The keychain answered with an error.
    Keychain,
    /// The header that carries the key was refused.
    Header(Refusal),
}

impl ResolveError {
    /// The stable name the settings dialog and the page key their text on. An address or header refusal gives the
    /// name of the [`Refusal`].
    pub fn as_str(self) -> &'static str {
        match self {
            ResolveError::UnknownProfile => "unknown_profile",
            ResolveError::Address(refusal) | ResolveError::Header(refusal) => refusal.as_str(),
            ResolveError::NeedsAddress => "needs_address",
            ResolveError::DisclosureNeeded => "disclosure_needed",
            ResolveError::KeyMissing(_) => "key_missing",
            ResolveError::KeyScope(_) => "key_scope",
            ResolveError::KeyDamaged(_) => "key_damaged",
            ResolveError::NoSecureStore => "no_secure_store",
            ResolveError::Keychain => "keychain",
        }
    }

    /// The key slot the error is about, if it is about one.
    pub fn slot(self) -> Option<KeySlot> {
        match self {
            ResolveError::KeyMissing(slot) | ResolveError::KeyScope(slot) | ResolveError::KeyDamaged(slot) => {
                Some(slot)
            }
            _ => None,
        }
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::UnknownProfile => f.write_str("there is no profile with this id"),
            ResolveError::Address(refusal) | ResolveError::Header(refusal) => fmt::Display::fmt(refusal, f),
            ResolveError::NeedsAddress => f.write_str("the address is empty or still has a part to fill in"),
            ResolveError::DisclosureNeeded => f.write_str("this address has not been acknowledged yet"),
            ResolveError::KeyMissing(_) => f.write_str("this provider needs a key and none is saved"),
            ResolveError::KeyScope(_) => f.write_str("the key was saved for another address; enter it again"),
            ResolveError::KeyDamaged(_) => f.write_str("the saved key is damaged; enter it again"),
            ResolveError::NoSecureStore => f.write_str("there is no system keychain, so the key cannot be used"),
            ResolveError::Keychain => f.write_str("the system keychain refused the request"),
        }
    }
}

impl std::error::Error for ResolveError {}

impl From<ResolveError> for AiError {
    fn from(error: ResolveError) -> Self {
        let detail = Detail::new(&error.to_string());
        match error {
            ResolveError::UnknownProfile => AiError::InvalidRequest(detail),
            ResolveError::KeyMissing(_) | ResolveError::KeyDamaged(_) => AiError::Auth(detail),
            ResolveError::NoSecureStore | ResolveError::Keychain => AiError::Unavailable(detail),
            ResolveError::Address(_)
            | ResolveError::NeedsAddress
            | ResolveError::DisclosureNeeded
            | ResolveError::KeyScope(_)
            | ResolveError::Header(_) => AiError::Policy(detail),
        }
    }
}

impl AiConfig {
    /// Makes the profile `id` and its keys into an endpoint, without a lookup and without a socket.
    ///
    /// In this order: the profile; its address, checked again by the rule of the settings boundary; the
    /// acknowledgement a gateway or third-party address asks for, once per address; the key of the provider slot,
    /// which is read from the keychain only when the profile says one was saved, and which must have been saved for
    /// exactly this address; the header that carries it, built with the address stored beside the key; for a preset
    /// with a gateway token, the token of the gateway in the same way. `local_only` is handed to the chokepoint, which
    /// decides on the resolved address at send time.
    pub fn resolve(&self, id: &str, vault: &KeyVault, local_only: bool) -> Result<Resolved, ResolveError> {
        let profile = self.get(id).ok_or(ResolveError::UnknownProfile)?;
        let target = profile.target().map_err(|error| match error {
            ProfileError::Address(refusal) => ResolveError::Address(refusal),
            _ => ResolveError::NeedsAddress,
        })?;
        let address = address_of(&target);
        if disclosure_for(profile.preset, &target).is_some() && !self.is_acknowledged(&address.scope) {
            return Err(ResolveError::DisclosureNeeded);
        }
        let provider_key = self.slot_header(profile, KeySlot::Provider, vault, target.origin())?;
        let gateway_key = if profile.preset.spec().gateway_token {
            self.slot_header(profile, KeySlot::Gateway, vault, target.origin())?
        } else {
            None
        };
        let mut endpoint = Endpoint::new(profile.wire, target, NetworkPolicy::new(profile.allow_lan_http, local_only));
        for secret in [provider_key, gateway_key].into_iter().flatten() {
            endpoint = endpoint.with_secret(secret);
        }
        Ok(Resolved {
            profile_id: profile.id.clone(),
            wire: profile.wire,
            model: profile.model.clone(),
            preset: profile.preset,
            address,
            endpoint,
        })
    }

    /// The header for one slot, or `None` when the slot is empty and may stay so.
    fn slot_header(
        &self,
        profile: &Profile,
        slot: KeySlot,
        vault: &KeyVault,
        origin: &Origin,
    ) -> Result<Option<SecretHeader>, ResolveError> {
        if !marker(profile, slot) {
            let needed = slot == KeySlot::Provider && profile.preset.spec().key == KeyNeed::Required;
            return if needed { Err(ResolveError::KeyMissing(slot)) } else { Ok(None) };
        }
        let stored = match vault.load(&account(&profile.id, slot)) {
            Ok(Some(stored)) => stored,
            Ok(None) => return Err(ResolveError::KeyMissing(slot)),
            Err(Fault::NoSecureStore) => return Err(ResolveError::NoSecureStore),
            Err(Fault::Store) => return Err(ResolveError::Keychain),
            Err(Fault::Damaged) => return Err(ResolveError::KeyDamaged(slot)),
        };
        if stored.origin != *origin {
            return Err(ResolveError::KeyScope(slot));
        }
        let (name, prefix) = match slot {
            KeySlot::Provider => auth_header_for(profile.wire),
            KeySlot::Gateway => ("cf-aig-authorization", "Bearer "),
        };
        SecretHeader::new(name, prefix, stored.key, stored.origin).map(Some).map_err(ResolveError::Header)
    }
}

// ---- what the preset adds to the request ----------------------------------------------------------------

/// An adapter wrapped by a preset that adds a part to every request body. Everything is delegated to the adapter; the
/// request body is parsed as a JSON object, the preset's object is merged into it member by member (nested objects
/// recursively, the preset's value replacing an equal-named value of the adapter) and the result is written back.
struct WithExtraBody {
    inner: Box<dyn WireAdapter>,
    /// The parsed object, or `None` when the table text is not an object: then no request is built, so a damaged table
    /// can never send a request without the part it should carry.
    extra: Option<Map<String, Value>>,
}

impl WithExtraBody {
    fn new(inner: Box<dyn WireAdapter>, extra: &str) -> Self {
        let extra = match serde_json::from_str::<Value>(extra) {
            Ok(Value::Object(object)) => Some(object),
            _ => None,
        };
        Self { inner, extra }
    }
}

/// Merges `extra` into `body`.
fn merge_object(body: &mut Map<String, Value>, extra: &Map<String, Value>) {
    for (name, value) in extra {
        match (body.get_mut(name), value) {
            (Some(Value::Object(existing)), Value::Object(added)) => merge_object(existing, added),
            _ => {
                body.insert(name.clone(), value.clone());
            }
        }
    }
}

impl WireAdapter for WithExtraBody {
    fn wire(&self) -> Wire {
        self.inner.wire()
    }

    fn capabilities(&self, model: &str, locality: Locality) -> Capabilities {
        self.inner.capabilities(model, locality)
    }

    fn request(&self, req: &ChatRequest, caps: &Capabilities) -> Result<WireRequest, AiError> {
        let mut request = self.inner.request(req, caps)?;
        let extra = self.extra.as_ref().ok_or_else(|| {
            AiError::InvalidRequest(Detail::new("the request options of this provider are not valid"))
        })?;
        let Ok(Value::Object(mut body)) = serde_json::from_slice::<Value>(&request.body) else {
            return Err(AiError::InvalidRequest(Detail::new("the request body is not a JSON object")));
        };
        merge_object(&mut body, extra);
        request.body = serde_json::to_vec(&body)
            .map_err(|_| AiError::InvalidRequest(Detail::new("the request body could not be written")))?;
        Ok(request)
    }

    fn turn(&self) -> Box<dyn WireTurn> {
        self.inner.turn()
    }

    fn rejection(&self, rejection: &Rejection, endpoint: &Endpoint) -> Option<AiError> {
        self.inner.rejection(rejection, endpoint)
    }
}

// ---- what the page may see ------------------------------------------------------------------------------

/// The first-use notice for an address that sends the prompt through a third party.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Disclosure {
    /// The host, as it is shown.
    pub host: String,
    /// Who the third party is.
    pub kind: DisclosureKind,
}

/// A profile as the settings dialog shows it. It is written out and never read back, and it has no field that could hold
/// a key: a key is a status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ProfileView {
    /// The id.
    pub id: String,
    /// The name.
    pub name: String,
    /// The kind of provider.
    pub preset: Preset,
    /// The wire.
    pub wire: Wire,
    /// The base address as saved.
    pub base_url: String,
    /// The model.
    pub model: String,
    /// Plain http to the local network is allowed.
    pub allow_lan_http: bool,
    /// The host as it is shown.
    pub host: String,
    /// The state of the provider key.
    pub key: KeyStatus,
    /// The state of the gateway token, for a preset that has one.
    pub gateway_key: Option<KeyStatus>,
    /// The notice to show before the first send, while the address is not acknowledged.
    pub disclosure: Option<Disclosure>,
}

impl AiConfig {
    /// The profiles as the dialog shows them, in file order.
    ///
    /// This reads the keychain for every profile whose marker says a key was saved, because the keychain has no way to
    /// ask whether an entry exists. It is meant for the moment the dialog opens and never for startup; a profile
    /// without a key does not touch the keychain.
    pub fn views(&self, vault: &KeyVault) -> Vec<ProfileView> {
        self.profiles
            .iter()
            .map(|profile| {
                let target = profile.target().ok();
                let host = target.as_ref().map(|target| address_of(target).host).unwrap_or_default();
                let disclosure = target.as_ref().and_then(|target| {
                    let kind = disclosure_for(profile.preset, target)?;
                    let acknowledged = self.is_acknowledged(&target.origin().scope());
                    (!acknowledged).then(|| Disclosure { host: host.clone(), kind })
                });
                ProfileView {
                    id: profile.id.clone(),
                    name: profile.name.clone(),
                    preset: profile.preset,
                    wire: profile.wire,
                    base_url: profile.base_url.clone(),
                    model: profile.model.clone(),
                    allow_lan_http: profile.allow_lan_http,
                    host,
                    key: vault.status(profile, KeySlot::Provider),
                    gateway_key: profile.preset.spec().gateway_token.then(|| vault.status(profile, KeySlot::Gateway)),
                    disclosure,
                }
            })
            .collect()
    }
}
