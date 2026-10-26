//! The passkey round trip through the pinned `webauthn-rs`, with a software authenticator and no browser.
//!
//! The relying party is built from one public URL the way the server will build it. Every challenge state is kept in a map
//! under a random id and taken out with `remove`: the library hands the state back on every call and does not forget it, so
//! the single-use rule belongs to the caller, and these tests are what proves it is needed. The credentials travel as JSON
//! text in both directions, in the form the page posts.

#[path = "support/webauthn_client.rs"]
mod webauthn_client;

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use webauthn_client::{SoftPasskey, Tweaks};
use webauthn_rs::prelude::{
    AuthenticationResult, DiscoverableAuthentication, DiscoverableKey, Passkey,
    PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential,
    Url, Uuid, Webauthn, WebauthnBuilder, WebauthnError,
};

const PUBLIC_URL: &str = "https://hw.example.test";

/// The relying party id and the origin, both taken from the one `public_url` string. The id is the host; the origin is the
/// scheme, the host and a non-default port, with no path, so that it is exactly what a browser puts in the client data.
fn relying_party(public_url: &str) -> (String, Url) {
    let url = Url::parse(public_url).expect("public_url parses");
    assert_eq!(url.scheme(), "https", "passkeys need https");
    let host = url.host_str().expect("public_url has a host").to_owned();
    let origin = Url::parse(&url.origin().ascii_serialization()).expect("origin parses");
    (host, origin)
}

fn webauthn_for(public_url: &str) -> (Webauthn, Url) {
    let (rp_id, origin) = relying_party(public_url);
    let webauthn = WebauthnBuilder::new(&rp_id, &origin)
        .expect("the host of public_url is the relying party id")
        .rp_name("holzWave")
        .build()
        .expect("the relying party builds");
    assert_eq!(
        webauthn.get_allowed_origins(),
        std::slice::from_ref(&origin)
    );
    (webauthn, origin)
}

/// Challenge states by random id. `take` removes: a state can be used once.
struct Challenges<S> {
    states: HashMap<String, S>,
}

impl<S> Challenges<S> {
    fn new() -> Self {
        Challenges {
            states: HashMap::new(),
        }
    }

    fn insert(&mut self, state: S) -> String {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).expect("the system random source answers");
        let id = URL_SAFE_NO_PAD.encode(raw);
        self.states.insert(id.clone(), state);
        id
    }

    fn take(&mut self, id: &str) -> Option<S> {
        self.states.remove(id)
    }

    fn len(&self) -> usize {
        self.states.len()
    }

    /// A copy of a state without using it up. A server must never do this; a test does it to show what the library alone
    /// would accept.
    fn copy_without_using(&self, id: &str) -> Option<S>
    where
        S: Clone,
    {
        self.states.get(id).cloned()
    }
}

/// Why finishing a ceremony failed: there was no state for the id (never issued, used already), the body was not a
/// credential, or the library refused it.
#[derive(Debug)]
#[allow(dead_code)] // the payloads are read through `Debug`, in the panic message of a failed assertion
enum FinishError {
    NoState,
    Malformed(serde_json::Error),
    Refused(WebauthnError),
}

struct Site {
    webauthn: Webauthn,
    origin: Url,
    registrations: Challenges<PasskeyRegistration>,
    authentications: Challenges<PasskeyAuthentication>,
    discoverables: Challenges<DiscoverableAuthentication>,
}

impl Site {
    fn new() -> Self {
        let (webauthn, origin) = webauthn_for(PUBLIC_URL);
        Site {
            webauthn,
            origin,
            registrations: Challenges::new(),
            authentications: Challenges::new(),
            discoverables: Challenges::new(),
        }
    }

    /// Start a registration; returns the state id and the options JSON for the page.
    fn start_registration(&mut self, handle: Uuid, name: &str) -> (String, Value) {
        let (options, state) = self
            .webauthn
            .start_passkey_registration(handle, name, name, None)
            .expect("registration starts");
        (
            self.registrations.insert(state),
            serde_json::to_value(&options).expect("options serialise"),
        )
    }

    /// Finish a registration from the JSON text the page posted.
    fn finish_registration(&mut self, id: &str, body: &str) -> Result<Passkey, FinishError> {
        let state = self.registrations.take(id).ok_or(FinishError::NoState)?;
        let credential: RegisterPublicKeyCredential =
            serde_json::from_str(body).map_err(FinishError::Malformed)?;
        self.webauthn
            .finish_passkey_registration(&credential, &state)
            .map_err(FinishError::Refused)
    }

    fn start_authentication(&mut self, passkeys: &[Passkey]) -> (String, Value) {
        let (options, state) = self
            .webauthn
            .start_passkey_authentication(passkeys)
            .expect("authentication starts");
        (
            self.authentications.insert(state),
            serde_json::to_value(&options).expect("options serialise"),
        )
    }

    fn finish_authentication(
        &mut self,
        id: &str,
        body: &str,
    ) -> Result<AuthenticationResult, FinishError> {
        let state = self.authentications.take(id).ok_or(FinishError::NoState)?;
        let credential: PublicKeyCredential =
            serde_json::from_str(body).map_err(FinishError::Malformed)?;
        self.webauthn
            .finish_passkey_authentication(&credential, &state)
            .map_err(FinishError::Refused)
    }

    /// Start a username-less authentication: no credential list, the person is identified by the response.
    fn start_discoverable(&mut self) -> (String, Value) {
        let (options, state) = self
            .webauthn
            .start_discoverable_authentication()
            .expect("discoverable authentication starts");
        (
            self.discoverables.insert(state),
            serde_json::to_value(&options).expect("options serialise"),
        )
    }

    /// Who does this response claim to be: the user handle and the credential id, read before anything is verified. The
    /// caller looks the account up with them and passes its credentials to `finish_discoverable`.
    fn identify_discoverable(&self, body: &str) -> Result<(Uuid, Vec<u8>), FinishError> {
        let credential: PublicKeyCredential =
            serde_json::from_str(body).map_err(FinishError::Malformed)?;
        self.webauthn
            .identify_discoverable_authentication(&credential)
            .map(|(handle, credential_id)| (handle, credential_id.to_vec()))
            .map_err(FinishError::Refused)
    }

    fn finish_discoverable(
        &mut self,
        id: &str,
        body: &str,
        keys: &[DiscoverableKey],
    ) -> Result<AuthenticationResult, FinishError> {
        let state = self.discoverables.take(id).ok_or(FinishError::NoState)?;
        let credential: PublicKeyCredential =
            serde_json::from_str(body).map_err(FinishError::Malformed)?;
        self.webauthn
            .finish_discoverable_authentication(&credential, state, keys)
            .map_err(FinishError::Refused)
    }

    /// A full registration with a fresh handle; returns the credential and the handle.
    fn register(&mut self, authenticator: &mut SoftPasskey) -> (Passkey, Uuid) {
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).expect("the system random source answers");
        let handle = Uuid::from_bytes(raw);
        let (id, options) = self.start_registration(handle, "alice");
        let response = authenticator
            .register(&options, &self.origin)
            .expect("the soft authenticator answers the registration");
        let passkey = self
            .finish_registration(&id, &response.to_string())
            .expect("the registration verifies");
        (passkey, handle)
    }
}

#[test]
fn registers_a_passkey_and_authenticates_with_it() {
    let mut site = Site::new();
    let mut authenticator = SoftPasskey::new();

    let (passkey, _handle) = site.register(&mut authenticator);
    assert_eq!(passkey.cred_id().as_slice(), authenticator.credential_id());
    assert_eq!(
        site.registrations.len(),
        0,
        "the registration state is used up"
    );

    let (id, options) = site.start_authentication(std::slice::from_ref(&passkey));
    assert_eq!(site.authentications.len(), 1);
    let response = authenticator
        .authenticate(&options, &site.origin)
        .expect("the soft authenticator answers the authentication");
    let result = site
        .finish_authentication(&id, &response.to_string())
        .expect("the authentication verifies");

    assert_eq!(result.cred_id(), passkey.cred_id());
    assert!(
        result.user_verified(),
        "the policy is user verification required"
    );
    assert_eq!(
        site.authentications.len(),
        0,
        "the authentication state is used up"
    );
}

/// The library's refusal, with the case printed so that a run with `--nocapture` shows what each negative check returned.
fn refused<T: std::fmt::Debug>(case: &str, result: Result<T, FinishError>) -> WebauthnError {
    match result {
        Err(FinishError::Refused(error)) => {
            println!("refused: {case}: {error:?}");
            error
        }
        other => panic!("{case}: expected the library to refuse, got {other:?}"),
    }
}

/// The signature counter stored in the credential, read from the JSON form of the `passkeys.passkey_json` column.
fn stored_counter(passkey: &Passkey) -> u64 {
    serde_json::to_value(passkey).expect("passkey serialises")["cred"]["counter"]
        .as_u64()
        .expect("the credential stores a counter")
}

/// The same credential with the stored counter set to `counter`, as a database row that is ahead of the authenticator.
fn with_stored_counter(passkey: &Passkey, counter: u64) -> Passkey {
    let mut value = serde_json::to_value(passkey).expect("passkey serialises");
    value["cred"]["counter"] = counter.into();
    serde_json::from_value(value).expect("passkey reads back")
}

#[test]
fn a_replayed_response_is_refused() {
    let mut site = Site::new();
    let mut authenticator = SoftPasskey::new();
    let (mut passkey, _handle) = site.register(&mut authenticator);
    assert_eq!(stored_counter(&passkey), 0);

    let (id, options) = site.start_authentication(std::slice::from_ref(&passkey));
    let response = authenticator
        .authenticate(&options, &site.origin)
        .expect("the authenticator answers")
        .to_string();
    let kept = site
        .authentications
        .copy_without_using(&id)
        .expect("the state is stored");

    let result = site
        .finish_authentication(&id, &response)
        .expect("the first use verifies");
    assert_eq!(result.counter(), 1);
    assert!(
        result.needs_update(),
        "the counter moved, so the row must be updated"
    );
    assert_eq!(passkey.update_credential(&result), Some(true));
    assert_eq!(
        stored_counter(&passkey),
        1,
        "the stored counter follows the authenticator"
    );

    // The same response, the same challenge state: the state was removed on first use.
    let again = site.finish_authentication(&id, &response);
    assert!(matches!(again, Err(FinishError::NoState)), "got {again:?}");
    println!("refused: same response, state already used: no state for the id");

    // The same response against a new challenge: the signed client data names the old challenge.
    let (fresh, _options) = site.start_authentication(std::slice::from_ref(&passkey));
    let error = refused(
        "same response, new challenge",
        site.finish_authentication(&fresh, &response),
    );
    assert!(
        matches!(error, WebauthnError::MismatchedChallenge),
        "got {error:?}"
    );

    // What the removal protects against: a state kept by mistake accepts the same response again, because the library
    // holds no memory of use. The single-use rule is the caller's, and it is what the two checks above prove.
    let credential: PublicKeyCredential = serde_json::from_str(&response).expect("response reads");
    assert!(
        site.webauthn
            .finish_passkey_authentication(&credential, &kept)
            .is_ok(),
        "a state that was kept would verify the replay"
    );

    // The authenticator's next answer verifies against the updated row and moves the counter on.
    let (id, options) = site.start_authentication(std::slice::from_ref(&passkey));
    let response = authenticator
        .authenticate(&options, &site.origin)
        .expect("answers")
        .to_string();
    let result = site
        .finish_authentication(&id, &response)
        .expect("the second use verifies");
    assert_eq!(result.counter(), 2);
    assert_eq!(passkey.update_credential(&result), Some(true));

    // An authenticator whose counter did not advance beyond the stored one (a cloned credential, or a row rolled forward):
    // the library reports it and refuses.
    let ahead = with_stored_counter(&passkey, 9);
    let (id, options) = site.start_authentication(std::slice::from_ref(&ahead));
    let response = authenticator
        .authenticate(&options, &site.origin)
        .expect("answers")
        .to_string();
    let error = refused(
        "counter 3 against a stored counter of 9",
        site.finish_authentication(&id, &response),
    );
    assert!(
        matches!(error, WebauthnError::CredentialPossibleCompromise),
        "got {error:?}"
    );
}

#[test]
fn a_response_for_another_origin_or_rp_is_refused() {
    let mut site = Site::new();
    let mut authenticator = SoftPasskey::new();
    let (passkey, _handle) = site.register(&mut authenticator);

    // The browser itself does not sign for an origin the relying party id does not cover.
    let evil = Url::parse("https://evil.example.test").expect("url parses");
    let (_id, options) = site.start_authentication(std::slice::from_ref(&passkey));
    assert!(
        authenticator.authenticate(&options, &evil).is_err(),
        "a correct browser refuses"
    );

    // A hostile client signs anyway. Every look-alike is refused by the exact origin match: another host, the relying
    // party id as a prefix of another domain, a subdomain, another port, another scheme.
    for foreign in [
        "https://evil.example.test",
        "https://hw.example.test.evil.test",
        "https://sub.hw.example.test",
        "https://hw.example.test:8443",
        "http://hw.example.test",
    ] {
        let foreign_origin = Url::parse(foreign).expect("url parses");
        let (id, options) = site.start_authentication(std::slice::from_ref(&passkey));
        let response = authenticator
            .authenticate_with(&options, &foreign_origin, &Tweaks::unchecked())
            .expect("the unchecked path signs")
            .to_string();
        let error = refused(
            &format!("origin {foreign}"),
            site.finish_authentication(&id, &response),
        );
        assert!(
            matches!(error, WebauthnError::InvalidRPOrigin),
            "{foreign}: got {error:?}"
        );
    }

    // Another relying party id with the right origin: the hash of the id is part of the signed authenticator data.
    for rp_id in [
        "example.test",
        "other.example.test",
        "hw.example.test.evil.test",
    ] {
        let (id, options) = site.start_authentication(std::slice::from_ref(&passkey));
        let tweaks = Tweaks {
            skip_browser_checks: true,
            rp_id: Some(rp_id.into()),
            ..Tweaks::default()
        };
        let response = authenticator
            .authenticate_with(&options, &site.origin, &tweaks)
            .expect("the unchecked path signs")
            .to_string();
        let error = refused(
            &format!("rp id {rp_id}"),
            site.finish_authentication(&id, &response),
        );
        assert!(
            matches!(error, WebauthnError::InvalidRPIDHash),
            "{rp_id}: got {error:?}"
        );
    }

    // The same two refusals at registration.
    let mut other = SoftPasskey::new();
    let (id, options) = site.start_registration(Uuid::from_bytes([7; 16]), "mallory");
    let response = other
        .register_with(&options, &evil, &Tweaks::unchecked())
        .expect("the unchecked path signs")
        .to_string();
    let error = refused(
        "registration, origin",
        site.finish_registration(&id, &response),
    );
    assert!(
        matches!(error, WebauthnError::InvalidRPOrigin),
        "got {error:?}"
    );

    let (id, options) = site.start_registration(Uuid::from_bytes([7; 16]), "mallory");
    let tweaks = Tweaks {
        skip_browser_checks: true,
        rp_id: Some("other.example.test".into()),
        ..Tweaks::default()
    };
    let response = other
        .register_with(&options, &site.origin, &tweaks)
        .expect("the unchecked path signs")
        .to_string();
    let error = refused(
        "registration, rp id",
        site.finish_registration(&id, &response),
    );
    assert!(
        matches!(error, WebauthnError::InvalidRPIDHash),
        "got {error:?}"
    );
}

#[test]
fn user_verification_is_required() {
    let mut site = Site::new();
    let mut authenticator = SoftPasskey::new();
    let (passkey, _handle) = site.register(&mut authenticator);

    // The authenticator answers as if the options had said `preferred`, and the person was not verified: the flag is not
    // set in the signed authenticator data. The server's policy is `required`.
    let (id, options) = site.start_authentication(std::slice::from_ref(&passkey));
    assert_eq!(options["publicKey"]["userVerification"], "required");
    let tweaks = Tweaks {
        user_verification: Some("preferred".into()),
        ..Tweaks::default()
    };
    let response = authenticator
        .authenticate_with(&options, &site.origin, &tweaks)
        .expect("the authenticator answers")
        .to_string();
    let error = refused(
        "authentication without user verification",
        site.finish_authentication(&id, &response),
    );
    assert!(
        matches!(error, WebauthnError::UserNotVerified),
        "got {error:?}"
    );

    // The same at registration.
    let mut other = SoftPasskey::new();
    let (id, options) = site.start_registration(Uuid::from_bytes([9; 16]), "mallory");
    assert_eq!(
        options["publicKey"]["authenticatorSelection"]["userVerification"],
        "required"
    );
    let response = other
        .register_with(&options, &site.origin, &tweaks)
        .expect("the authenticator answers")
        .to_string();
    let error = refused(
        "registration without user verification",
        site.finish_registration(&id, &response),
    );
    assert!(
        matches!(error, WebauthnError::UserNotVerified),
        "got {error:?}"
    );
}

#[test]
fn a_discoverable_authentication_identifies_the_account_handle() {
    let mut site = Site::new();
    let mut authenticator = SoftPasskey::new();
    let (passkey, handle) = site.register(&mut authenticator);
    let accounts: HashMap<Uuid, Passkey> = HashMap::from([(handle, passkey.clone())]);

    let (id, options) = site.start_discoverable();
    assert_eq!(
        options["publicKey"]["allowCredentials"],
        serde_json::json!([])
    );
    assert_eq!(options["publicKey"]["userVerification"], "required");
    // The library marks the request for the browser's autofill; a sign-in button leaves that field out.
    println!(
        "discoverable options carry mediation: {}",
        options["mediation"]
    );

    let response = authenticator
        .authenticate(&options, &site.origin)
        .expect("the authenticator picks its credential")
        .to_string();
    let (identified, credential_id) = site
        .identify_discoverable(&response)
        .expect("a handle is present");
    assert_eq!(
        identified.as_bytes(),
        handle.as_bytes(),
        "the handle is the one given at registration"
    );
    assert_eq!(credential_id, authenticator.credential_id());

    let account_key = accounts
        .get(&identified)
        .expect("the handle names an account");
    let result = site
        .finish_discoverable(&id, &response, &[DiscoverableKey::from(account_key)])
        .expect("the discoverable authentication verifies");
    assert_eq!(result.cred_id(), passkey.cred_id());
    assert!(result.user_verified());

    // A response without a user handle cannot identify anyone.
    let (id, options) = site.start_discoverable();
    let mut response = authenticator
        .authenticate(&options, &site.origin)
        .expect("answers");
    response["response"]
        .as_object_mut()
        .expect("response object")
        .remove("userHandle");
    let error = refused(
        "no user handle",
        site.identify_discoverable(&response.to_string()),
    );
    assert!(
        matches!(error, WebauthnError::InvalidUserUniqueId),
        "got {error:?}"
    );
    assert!(site.discoverables.take(&id).is_some());

    // A handle that points at someone else's credentials: the credential in the response is not among them.
    let mut stranger = SoftPasskey::new();
    let (stranger_key, _) = site.register(&mut stranger);
    let (id, options) = site.start_discoverable();
    let response = authenticator
        .authenticate(&options, &site.origin)
        .expect("answers")
        .to_string();
    let error = refused(
        "credential of another account",
        site.finish_discoverable(&id, &response, &[DiscoverableKey::from(&stranger_key)]),
    );
    assert!(
        matches!(error, WebauthnError::CredentialNotFound),
        "got {error:?}"
    );
}

/// What happened to a credential row that was damaged before it was read.
#[derive(Debug)]
#[allow(dead_code)] // the library's error is read through `Debug`, in the printed outcome
enum Outcome {
    NotLoaded,
    ClientRefused,
    Refused(WebauthnError),
    Authenticated,
}

/// Load `row` as the stored form and try to sign in with it against an authenticator that holds the original credential.
fn sign_in_with_stored(site: &mut Site, authenticator: &mut SoftPasskey, row: &str) -> Outcome {
    let Ok(passkey) = serde_json::from_str::<Passkey>(row) else {
        return Outcome::NotLoaded;
    };
    let (id, options) = site.start_authentication(std::slice::from_ref(&passkey));
    let Ok(response) = authenticator.authenticate(&options, &site.origin) else {
        return Outcome::ClientRefused;
    };
    match site.finish_authentication(&id, &response.to_string()) {
        Ok(_) => Outcome::Authenticated,
        Err(FinishError::Refused(error)) => Outcome::Refused(error),
        Err(other) => panic!("unexpected {other:?}"),
    }
}

/// `row` with the byte at `at` replaced by `with`; the result stays valid UTF-8 because every touched byte is ASCII.
fn with_byte(row: &str, at: usize, with: u8) -> String {
    let mut bytes = row.as_bytes().to_vec();
    assert!(bytes[at].is_ascii() && with.is_ascii());
    bytes[at] = with;
    String::from_utf8(bytes).expect("ascii stays utf-8")
}

#[test]
fn a_stored_passkey_authenticates_and_a_damaged_one_does_not() {
    let mut site = Site::new();
    let mut authenticator = SoftPasskey::new();
    let (passkey, _handle) = site.register(&mut authenticator);

    // The form of the `passkeys.passkey_json` column: text, read back, authenticates.
    let row = serde_json::to_string(&passkey).expect("passkey serialises");
    let outcome = sign_in_with_stored(&mut site, &mut authenticator, &row);
    assert!(matches!(outcome, Outcome::Authenticated), "got {outcome:?}");

    // One byte of the structure: the row does not parse.
    let outcome = sign_in_with_stored(&mut site, &mut authenticator, &with_byte(&row, 0, b'['));
    println!("damaged row, first byte: {outcome:?}");
    assert!(matches!(outcome, Outcome::NotLoaded), "got {outcome:?}");

    // One byte of the credential id: the row reads, but the credential it names is not the one the authenticator holds.
    let at = row.find("\"cred_id\":\"").expect("the id is stored") + "\"cred_id\":\"".len() + 4;
    let replacement = if row.as_bytes()[at] == b'A' {
        b'B'
    } else {
        b'A'
    };
    let outcome = sign_in_with_stored(
        &mut site,
        &mut authenticator,
        &with_byte(&row, at, replacement),
    );
    println!("damaged row, credential id: {outcome:?}");
    assert!(
        !matches!(outcome, Outcome::Authenticated),
        "got {outcome:?}"
    );

    // One byte of the public key: the row reads, the options name the right credential, the signature no longer verifies.
    let at = row.find("\"x\":\"").expect("the key is stored") + "\"x\":\"".len() + 10;
    let replacement = if row.as_bytes()[at] == b'A' {
        b'B'
    } else {
        b'A'
    };
    let outcome = sign_in_with_stored(
        &mut site,
        &mut authenticator,
        &with_byte(&row, at, replacement),
    );
    println!("damaged row, public key: {outcome:?}");
    assert!(
        !matches!(outcome, Outcome::Authenticated),
        "got {outcome:?}"
    );
}

/// The PRF output of the browser's `prf` extension, as the page sends it in `clientExtensionResults`. The bytes are test
/// data and key nothing.
fn prf_client_results(output: &[u8; 32]) -> Value {
    serde_json::json!({ "prf": { "enabled": true, "results": { "first": URL_SAFE_NO_PAD.encode(output) } } })
}

/// The PRF output the way the server reads it: from the raw JSON the page posted, not from the library's type.
fn read_prf_first(body: &Value) -> Option<Vec<u8>> {
    let first = body["clientExtensionResults"]["prf"]["results"]["first"].as_str()?;
    URL_SAFE_NO_PAD.decode(first).ok()
}

/// Ask for the PRF evaluation with `salt` by editing the options JSON the library produced. Creates `extensions` when the
/// library left it out and keeps whatever it put there.
fn request_prf(options: &mut Value, salt: &[u8; 32]) {
    let public_key = options["publicKey"]
        .as_object_mut()
        .expect("options carry publicKey");
    let extensions = public_key
        .entry("extensions")
        .or_insert_with(|| serde_json::json!({}));
    extensions["prf"] = serde_json::json!({ "eval": { "first": URL_SAFE_NO_PAD.encode(salt) } });
}

#[test]
fn a_response_with_prf_client_results_verifies_like_one_without() {
    let mut site = Site::new();
    let mut authenticator = SoftPasskey::new();
    let (passkey, _handle) = site.register(&mut authenticator);

    let (id, options) = site.start_authentication(std::slice::from_ref(&passkey));
    let plain = authenticator
        .authenticate(&options, &site.origin)
        .expect("the authenticator answers");
    assert_eq!(plain["clientExtensionResults"], serde_json::json!({}));
    let state = site.authentications.take(&id).expect("the state is stored");

    let output = [0x5a; 32];
    let mut with_prf = plain.clone();
    with_prf["clientExtensionResults"] = prf_client_results(&output);

    // The same state verifies both bodies: verifying does not change the state, and the extension data is not signed.
    let verify = |body: &Value| {
        let credential: PublicKeyCredential =
            serde_json::from_str(&body.to_string()).expect("the library type reads the body");
        site.webauthn
            .finish_passkey_authentication(&credential, &state)
            .expect("the response verifies")
    };
    let without = verify(&plain);
    let with = verify(&with_prf);
    assert_eq!(with.cred_id(), without.cred_id());
    assert_eq!(with.counter(), without.counter());
    assert_eq!(with.user_verified(), without.user_verified());
    assert_eq!(with.needs_update(), without.needs_update());
    assert_eq!(with.backup_state(), without.backup_state());

    // The server reads the PRF bytes from the raw JSON; the library's type does not carry them.
    assert_eq!(read_prf_first(&with_prf).as_deref(), Some(&output[..]));
    assert_eq!(read_prf_first(&plain), None);
    let typed: PublicKeyCredential = serde_json::from_str(&with_prf.to_string()).expect("reads");
    let typed = serde_json::to_value(&typed).expect("serialises");
    assert!(
        typed["extensions"].get("prf").is_none(),
        "the typed form drops the PRF result"
    );

    // The result is unsigned: a different value verifies too. The key derivation therefore must not trust it by itself;
    // the output only unwraps a key the person owns, and a forged value unwraps nothing.
    let mut forged = plain.clone();
    forged["clientExtensionResults"] = prf_client_results(&[0xa5; 32]);
    assert_eq!(verify(&forged).cred_id(), without.cred_id());
    assert_eq!(read_prf_first(&forged).as_deref(), Some(&[0xa5u8; 32][..]));
}

#[test]
fn the_options_can_carry_a_prf_salt() {
    let mut site = Site::new();
    let mut authenticator = SoftPasskey::new();
    let (passkey, handle) = site.register(&mut authenticator);
    let salt = [0xc3; 32];

    // A sign-in with a known credential: the library leaves `extensions` out, so the edit creates it.
    let (id, options) = site.start_authentication(std::slice::from_ref(&passkey));
    assert!(options["publicKey"].get("extensions").is_none());
    let mut patched = options.clone();
    request_prf(&mut patched, &salt);
    assert_eq!(
        patched["publicKey"]["challenge"],
        options["publicKey"]["challenge"]
    );
    assert_eq!(
        patched["publicKey"]["extensions"]["prf"]["eval"]["first"],
        URL_SAFE_NO_PAD.encode(salt)
    );
    // Still options for the library's type, and the typed form would lose the request: the server must send the edited raw
    // JSON, not a value it parsed and wrote again.
    let typed: webauthn_rs::prelude::RequestChallengeResponse =
        serde_json::from_value(patched.clone()).expect("the edited options still read as options");
    let retyped = serde_json::to_value(&typed).expect("serialises");
    assert!(
        retyped["publicKey"]
            .get("extensions")
            .and_then(|e| e.get("prf"))
            .is_none()
    );

    let response = authenticator
        .authenticate(&patched, &site.origin)
        .expect("the authenticator answers the edited options");
    let result = site
        .finish_authentication(&id, &response.to_string())
        .expect("the response verifies against the original state");
    assert_eq!(result.cred_id(), passkey.cred_id());

    // A username-less sign-in: the library already sends extensions; the edit adds to them.
    let (id, options) = site.start_discoverable();
    let mut patched = options.clone();
    request_prf(&mut patched, &salt);
    assert_eq!(
        patched["publicKey"]["extensions"]["uvm"],
        options["publicKey"]["extensions"]["uvm"]
    );
    assert_eq!(
        patched["publicKey"]["extensions"]["prf"]["eval"]["first"],
        URL_SAFE_NO_PAD.encode(salt)
    );
    let response = authenticator
        .authenticate(&patched, &site.origin)
        .expect("the authenticator answers the edited options")
        .to_string();
    let (identified, _credential_id) = site
        .identify_discoverable(&response)
        .expect("a handle is present");
    assert_eq!(identified, handle);
    let result = site
        .finish_discoverable(&id, &response, &[DiscoverableKey::from(&passkey)])
        .expect("the response verifies against the original state");
    assert_eq!(result.cred_id(), passkey.cred_id());
}

#[test]
fn registration_accepts_prf_enabled() {
    let mut site = Site::new();
    let mut authenticator = SoftPasskey::new();
    let (id, mut options) = site.start_registration(Uuid::from_bytes([5; 16]), "alice");

    // The page asks whether the authenticator supports PRF while it creates the credential.
    let public_key = options["publicKey"]
        .as_object_mut()
        .expect("options carry publicKey");
    public_key["extensions"]["prf"] = serde_json::json!({});

    let mut response = authenticator
        .register(&options, &site.origin)
        .expect("the authenticator answers");
    response["clientExtensionResults"] =
        serde_json::json!({ "credProps": { "rk": true }, "prf": { "enabled": true } });
    let passkey = site
        .finish_registration(&id, &response.to_string())
        .expect("a registration that reports PRF support verifies");
    assert_eq!(passkey.cred_id().as_slice(), authenticator.credential_id());
    assert_eq!(response["clientExtensionResults"]["prf"]["enabled"], true);
}
