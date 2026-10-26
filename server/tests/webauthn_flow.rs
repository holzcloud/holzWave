//! The passkey round trip through the pinned `webauthn-rs`, with a software authenticator and no browser.
//!
//! The relying party is built from one public URL the way the server will build it. Every challenge state is kept in a map
//! under a random id and taken out with `remove`: the library hands the state back on every call and does not forget it, so
//! the single-use rule belongs to the caller, and these tests are what proves it is needed. The credentials travel as JSON
//! text in both directions, in the form the page posts.

#[path = "support/webauthn_client.rs"]
mod webauthn_client;

use std::collections::HashMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value;
use webauthn_client::SoftPasskey;
use webauthn_rs::prelude::{
    AuthenticationResult, Passkey, PasskeyAuthentication, PasskeyRegistration,
    PublicKeyCredential, RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder,
    WebauthnError,
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
    assert_eq!(webauthn.get_allowed_origins(), [origin.clone()]);
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
}

/// Why finishing a ceremony failed: there was no state for the id (never issued, used already), the body was not a
/// credential, or the library refused it.
#[derive(Debug)]
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
}

impl Site {
    fn new() -> Self {
        let (webauthn, origin) = webauthn_for(PUBLIC_URL);
        Site {
            webauthn,
            origin,
            registrations: Challenges::new(),
            authentications: Challenges::new(),
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
    assert_eq!(site.registrations.len(), 0, "the registration state is used up");

    let (id, options) = site.start_authentication(std::slice::from_ref(&passkey));
    assert_eq!(site.authentications.len(), 1);
    let response = authenticator
        .authenticate(&options, &site.origin)
        .expect("the soft authenticator answers the authentication");
    let result = site
        .finish_authentication(&id, &response.to_string())
        .expect("the authentication verifies");

    assert_eq!(result.cred_id(), passkey.cred_id());
    assert!(result.user_verified(), "the policy is user verification required");
    assert_eq!(site.authentications.len(), 0, "the authentication state is used up");
}
