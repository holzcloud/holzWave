//! A software passkey that answers the way a browser and an authenticator do, for the passkey tests.
//!
//! It owns one credential. It takes the options JSON the server sends to the page and returns the credential JSON the page
//! posts back, so a test goes through serialisation in both directions. The signing is done by the soft authenticator of
//! `webauthn-authenticator-rs`; the keys exist only inside the test process and are never printed.
//!
//! What the soft authenticator cannot do, this helper does around it, and says so:
//!
//! - It refuses a discoverable request (an empty allow list). A resident credential answers such a request on its own, so the
//!   helper fills the allow list with the one credential it holds and returns the user handle it stored at registration. The
//!   user handle is not signed; a real authenticator returns it from the resident credential in the same way.
//! - It sets the user-verified flag only when the options ask for it (`userVerification: required`), and refuses to answer
//!   such an options object unless it was built to claim verification. The tests build it to claim it, because a real device
//!   would have verified the person.
//! - The browser compares the origin with the relying party id before it signs. The helper does the same on the checked
//!   path and skips that comparison on the unchecked path, which is how an attacker's page or a hostile client produces a
//!   response for another origin or another relying party id.

#![allow(dead_code)]

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use webauthn_authenticator_rs::error::WebauthnCError;
use webauthn_authenticator_rs::softpasskey::SoftPasskey as Backend;
use webauthn_authenticator_rs::{AuthenticatorBackend, WebauthnAuthenticator};
use webauthn_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse, Url,
};

/// How long a browser lets the person take; the soft authenticator ignores it.
const TIMEOUT_MS: u32 = 60_000;

/// Deviations from what a correct browser sends, for the negative tests. The default is a correct browser.
#[derive(Debug, Clone, Default)]
pub struct Tweaks {
    /// Sign for the given origin without the browser's check that the origin matches the relying party id.
    pub skip_browser_checks: bool,
    /// Sign for this relying party id instead of the one in the options.
    pub rp_id: Option<String>,
    /// Answer as if the options had asked for this user verification policy (`required`, `preferred`, `discouraged`).
    pub user_verification: Option<String>,
}

impl Tweaks {
    /// A response for another origin, with the browser's own check out of the way.
    pub fn unchecked() -> Self {
        Tweaks {
            skip_browser_checks: true,
            ..Tweaks::default()
        }
    }
}

/// One soft authenticator with one credential.
pub struct SoftPasskey {
    backend: Backend,
    credential_id: Option<Vec<u8>>,
    user_handle: Option<Vec<u8>>,
}

impl Default for SoftPasskey {
    fn default() -> Self {
        Self::new()
    }
}

impl SoftPasskey {
    /// An authenticator that reports user verification when the options ask for it.
    pub fn new() -> Self {
        SoftPasskey {
            backend: Backend::new(true),
            credential_id: None,
            user_handle: None,
        }
    }

    /// The id of the credential created by [`SoftPasskey::register`].
    pub fn credential_id(&self) -> &[u8] {
        self.credential_id.as_deref().expect("no credential yet")
    }

    /// Registration the way a correct browser does it. `options` is the JSON the server sends (`{"publicKey": ...}`).
    pub fn register(&mut self, options: &Value, origin: &Url) -> Result<Value, WebauthnCError> {
        self.register_with(options, origin, &Tweaks::default())
    }

    pub fn register_with(
        &mut self,
        options: &Value,
        origin: &Url,
        tweaks: &Tweaks,
    ) -> Result<Value, WebauthnCError> {
        let mut options = options.clone();
        if let Some(rp_id) = &tweaks.rp_id {
            options["publicKey"]["rp"]["id"] = json!(rp_id);
        }
        if let Some(policy) = &tweaks.user_verification {
            options["publicKey"]["authenticatorSelection"]["userVerification"] = json!(policy);
        }
        let creation: CreationChallengeResponse =
            serde_json::from_value(options).map_err(|_| WebauthnCError::Json)?;
        let user_handle = creation.public_key.user.id.clone();

        let credential: RegisterPublicKeyCredential = if tweaks.skip_browser_checks {
            self.backend
                .perform_register(origin.clone(), creation.public_key, TIMEOUT_MS)?
        } else {
            self.backend.do_registration(origin.clone(), creation)?
        };

        self.credential_id = Some(credential.raw_id.clone());
        self.user_handle = Some(user_handle);
        browser_form(serde_json::to_value(&credential).map_err(|_| WebauthnCError::Json)?)
    }

    /// Authentication the way a correct browser does it. An empty allow list is a discoverable request: the credential is
    /// chosen by the authenticator and the answer carries the user handle.
    pub fn authenticate(&mut self, options: &Value, origin: &Url) -> Result<Value, WebauthnCError> {
        self.authenticate_with(options, origin, &Tweaks::default())
    }

    pub fn authenticate_with(
        &mut self,
        options: &Value,
        origin: &Url,
        tweaks: &Tweaks,
    ) -> Result<Value, WebauthnCError> {
        let mut options = options.clone();
        let public_key = options
            .get_mut("publicKey")
            .and_then(Value::as_object_mut)
            .ok_or(WebauthnCError::Json)?;

        let discoverable = public_key
            .get("allowCredentials")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty);
        if discoverable {
            let id = self
                .credential_id
                .as_ref()
                .ok_or(WebauthnCError::Internal)?;
            public_key.insert(
                "allowCredentials".into(),
                json!([{ "type": "public-key", "id": URL_SAFE_NO_PAD.encode(id) }]),
            );
        }
        if let Some(rp_id) = &tweaks.rp_id {
            public_key.insert("rpId".into(), json!(rp_id));
        }
        if let Some(policy) = &tweaks.user_verification {
            public_key.insert("userVerification".into(), json!(policy));
        }

        let request: RequestChallengeResponse =
            serde_json::from_value(options).map_err(|_| WebauthnCError::Json)?;
        let mut credential: PublicKeyCredential = if tweaks.skip_browser_checks {
            self.backend
                .perform_auth(origin.clone(), request.public_key, TIMEOUT_MS)?
        } else {
            self.backend.do_authentication(origin.clone(), request)?
        };

        if discoverable {
            credential.response.user_handle = self.user_handle.clone();
        }
        browser_form(serde_json::to_value(&credential).map_err(|_| WebauthnCError::Json)?)
    }
}

/// The library serialises its response types with the field name `extensions`; the browser's `toJSON()` form names it
/// `clientExtensionResults`, holds only the extensions that answered, and leaves out an absent `userHandle`. The page posts the browser's form.
fn browser_form(mut credential: Value) -> Result<Value, WebauthnCError> {
    let object = credential.as_object_mut().ok_or(WebauthnCError::Json)?;
    let mut extensions = object.remove("extensions").unwrap_or_else(|| json!({}));
    if let Some(entries) = extensions.as_object_mut() {
        entries.retain(|_, value| !value.is_null());
    }
    object.insert("clientExtensionResults".into(), extensions);
    if let Some(response) = object.get_mut("response").and_then(Value::as_object_mut)
        && response.get("userHandle").is_some_and(Value::is_null)
    {
        response.remove("userHandle");
    }
    Ok(credential)
}
