//! Android only: the one-time set-up of the platform certificate verifier.
//!
//! The WebDAV client of the sync (`sync::webdav`) and the client of the AI chat (`ai::http`) both take their trust decisions
//! from `rustls-platform-verifier`. On Android that crate calls into the Java side and needs the Android context once, before
//! the first request; this is its function for that, handed on so the app shell can call it without a second direct dependency
//! on the verifier. The `Env` type in its signature is the one of `jni` 0.22, the version the app names.

pub use rustls_platform_verifier::android::init_with_env;
