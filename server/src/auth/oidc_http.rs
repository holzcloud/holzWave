//! The outgoing HTTP client of the OpenID Connect flow, with its address, size and time limits.
//!
//! The library that speaks the protocol (`openidconnect`) asks for one thing from its surroundings: a client that turns a
//! request into a response. [`HttpError`] and [`HttpFuture`] are the shape of that answer. Every failure is one of a few
//! fixed words, so that no address, header or body of a provider ends up in an error text.
//!
//! [`HttpsClient`] is the production client. It is built with the core's client builder, so the TLS stack is the same as the
//! desktop's (`rustls` with the `ring` provider and the platform's certificate verifier) and so are the rules that no redirect
//! is followed, no proxy setting of the environment is used and the address must be `https`. On top of that it sends no
//! cookie and keeps none (the cookie store of the HTTP crate is not compiled), allows ten seconds for the connection and ten
//! seconds in total for a request, and reads at most one MiB of a response: a longer answer is cut off as soon as the limit is
//! passed, and the rest is never buffered. The client secret reader is at the end of the file.

use std::fmt;
use std::future::Future;
use std::io::Read;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use holzwave_core::ai::http::{ClientOptions, ProxyPolicy, build_client};
use openidconnect::{AsyncHttpClient, HttpRequest, HttpResponse};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::keyfile::{open_private, shown};

/// Why a request to an identity provider did not give a response. No variant holds an address, a header or a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpError {
    /// The address is not `https`, and the client only speaks `https`.
    NotHttps,
    /// The response is larger than the limit.
    TooLarge,
    /// The provider did not answer in time.
    TimedOut,
    /// The request could not be made or the answer could not be read.
    Failed,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::NotHttps => f.write_str("the identity provider address is not https"),
            HttpError::TooLarge => f.write_str("the identity provider answer is larger than the limit"),
            HttpError::TimedOut => f.write_str("the identity provider did not answer in time"),
            HttpError::Failed => f.write_str("the identity provider could not be reached"),
        }
    }
}

impl std::error::Error for HttpError {}

/// What a client of the flow returns: a boxed future that can move between threads.
pub type HttpFuture<'c> = Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'c>>;

// ---------------------------------------------------------------------------------------------------------------------
// The client
// ---------------------------------------------------------------------------------------------------------------------

/// How long a connection to a provider may take, the TCP connect and the TLS handshake together.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a whole request to a provider may take, from the connection to the last byte of the answer.
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);

/// The most bytes of a response that are read: one MiB.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// The limits of a client. [`Limits::PRODUCTION`] is what the server uses; a test makes them smaller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The bound on one connection attempt.
    pub connect: Duration,
    /// The bound on a whole request.
    pub total: Duration,
    /// The most bytes of an answer.
    pub max_bytes: usize,
}

impl Limits {
    /// Ten seconds to connect, ten seconds in total, one MiB.
    pub const PRODUCTION: Limits =
        Limits { connect: CONNECT_TIMEOUT, total: TOTAL_TIMEOUT, max_bytes: MAX_RESPONSE_BYTES };
}

/// The HTTP client the server reaches identity providers with.
#[derive(Clone)]
pub struct HttpsClient {
    client: reqwest::Client,
    https_only: bool,
    limits: Limits,
}

impl fmt::Debug for HttpsClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HttpsClient(..)")
    }
}

impl HttpsClient {
    /// The production client: `https` only when `https_only` (production passes `true`), the limits of
    /// [`Limits::PRODUCTION`]. Building opens no socket.
    pub fn new(https_only: bool) -> Result<HttpsClient, HttpError> {
        HttpsClient::with_limits(https_only, Limits::PRODUCTION)
    }

    /// A client with other limits, for tests.
    pub fn with_limits(https_only: bool, limits: Limits) -> Result<HttpsClient, HttpError> {
        let mut options = ClientOptions::new(https_only, ProxyPolicy::Direct);
        options.connect_timeout = limits.connect;
        options.read_timeout = limits.total;
        let client = build_client(&options).map_err(|_| HttpError::Failed)?;
        Ok(HttpsClient { client, https_only, limits })
    }

    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        match request.uri().scheme_str() {
            Some("https") => {}
            // Refused before a connection exists.
            Some("http") if !self.https_only => {}
            Some("http") => return Err(HttpError::NotHttps),
            _ => return Err(HttpError::Failed),
        }
        let mut request = reqwest::Request::try_from(request).map_err(|_| HttpError::Failed)?;
        *request.timeout_mut() = Some(self.limits.total);
        let mut response = self.client.execute(request).await.map_err(classify)?;
        if response.content_length().is_some_and(|length| length > self.limits.max_bytes as u64) {
            return Err(HttpError::TooLarge);
        }
        let mut body = Vec::new();
        // One chunk at a time: the limit is checked before a chunk is kept, and dropping the response closes the connection.
        while let Some(chunk) = response.chunk().await.map_err(classify)? {
            if body.len().saturating_add(chunk.len()) > self.limits.max_bytes {
                return Err(HttpError::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        let mut answer = openidconnect::http::Response::builder().status(response.status());
        if let Some(headers) = answer.headers_mut() {
            *headers = response.headers().clone();
        }
        answer.body(body).map_err(|_| HttpError::Failed)
    }
}

/// The word for a failed request. Nothing of the error text is kept: it can hold the address.
fn classify(error: reqwest::Error) -> HttpError {
    if error.is_timeout() { HttpError::TimedOut } else { HttpError::Failed }
}

impl<'c> AsyncHttpClient<'c> for HttpsClient {
    type Error = HttpError;
    type Future = HttpFuture<'c>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(self.send(request))
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The client secret
// ---------------------------------------------------------------------------------------------------------------------

/// The largest client secret file that is read, in bytes. Anything bigger is not a client secret.
pub const MAX_SECRET_FILE_BYTES: u64 = 4_096;

/// What the sentences call the file.
const SECRET_FILE: &str = "client secret file";

/// What the `Debug` text of a client secret prints.
const SECRET_DEBUG: &str = "ClientSecretText(..)";

/// A refusal to use a client secret file: one sentence that names the file and the problem. It never holds file content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFileError(String);

impl SecretFileError {
    fn new(sentence: String) -> SecretFileError {
        SecretFileError(sentence)
    }

    /// The sentence.
    pub fn sentence(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SecretFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SecretFileError {}

/// The client secret of a provider. Not `Clone`, a marker for `Debug`, wiped when dropped.
pub struct ClientSecretText(Zeroizing<String>);

impl ClientSecretText {
    /// The secret, for the one place that authenticates the token request.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ClientSecretText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(SECRET_DEBUG)
    }
}

impl ZeroizeOnDrop for ClientSecretText {}

/// Reads the client secret at `path`: the file is opened through the private-file opener of the key module (a link, a file
/// that is not a regular file and a file with a permission bit for the group's write or for others are refused; the owner is
/// not checked, because a mounted secret belongs to another user), at most [`MAX_SECRET_FILE_BYTES`] are read, one trailing
/// newline is dropped, and a file that is empty, too large or not UTF-8 is refused. A refusal is a sentence that names the
/// file and the problem, never its content.
pub fn read_client_secret(path: &Path) -> Result<ClientSecretText, SecretFileError> {
    let file = open_private(path, SECRET_FILE).map_err(|e| SecretFileError::new(e.sentence().to_owned()))?;
    let mut content = Zeroizing::new(Vec::new());
    (&file).take(MAX_SECRET_FILE_BYTES + 1).read_to_end(&mut content).map_err(|e| {
        SecretFileError::new(format!("the {SECRET_FILE} {} cannot be read ({})", shown(path), e.kind()))
    })?;
    if content.len() as u64 > MAX_SECRET_FILE_BYTES {
        return Err(SecretFileError::new(format!(
            "the {SECRET_FILE} {} is larger than {MAX_SECRET_FILE_BYTES} bytes, which is not a client secret",
            shown(path)
        )));
    }
    let text = std::str::from_utf8(&content).map_err(|_| {
        SecretFileError::new(format!("the {SECRET_FILE} {} is not text; write the secret as plain text", shown(path)))
    })?;
    let secret = Zeroizing::new(text.strip_suffix('\n').unwrap_or(text).to_owned());
    content.zeroize();
    if secret.is_empty() {
        return Err(SecretFileError::new(format!("the {SECRET_FILE} {} is empty; put the client secret in it", shown(path))));
    }
    Ok(ClientSecretText(secret))
}
