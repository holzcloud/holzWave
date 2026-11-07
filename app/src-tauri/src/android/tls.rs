//! The set-up of the platform certificate verifier on Android.
//!
//! Both network clients of the core (the WebDAV client of the sync and the client of the AI chat) take their trust decisions
//! from `rustls-platform-verifier`. On Android the verifier calls into the Java side, so it has to be handed the Android
//! context once, before the first request, and the app has to carry its small Kotlin component (the Gradle wiring in
//! `gen/android/app/build.gradle.kts`). The Kotlin class [`HolzwavePlugin`](super::plugin) calls the one function below when
//! it is made, which is before any network code can run.
//!
//! A failed set-up never crashes the app and never loosens a check. The function catches every error, leaves the readiness
//! flag of the host unset, and the two places that start an HTTPS request refuse with the fixed word `tls_not_ready`
//! (`holzwave_host::tls_ready`). There is no other verifier in the app and no fallback to the roots of a bundled list.

use jni::EnvUnowned;
use jni::errors::LogErrorAndDefault;
use jni::jni_mangle;
use jni::objects::JObject;

/// What the set-up reports to the log (stderr reaches logcat as `RustStdoutStderr`). Fixed words; the error of the system is
/// not printed because it may name a class or a path of the device.
const OK_LINE: &str = "holzwave: the platform certificate verifier is set up";
const FAILED_LINE: &str = "holzwave: the platform certificate verifier could not be set up; HTTPS requests are refused";

/// Called from Kotlin: `HolzwavePlugin.initTls(context)`. Idempotent: a second call finds the verifier set up and changes nothing.
#[jni_mangle("ch.holzcloud.holzwave.HolzwavePlugin", "initTls")]
pub fn init_tls<'caller>(mut unowned_env: EnvUnowned<'caller>, _this: JObject<'caller>, context: JObject<'caller>) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            match holzwave_core::platform_tls::init_with_env(env, context) {
                Ok(()) => {
                    holzwave_host::tls_ready::global().set();
                    eprintln!("{OK_LINE}");
                }
                Err(_) => {
                    // A Java exception the failed call left pending would be thrown into Kotlin when this returns.
                    env.exception_clear();
                    eprintln!("{FAILED_LINE}");
                }
            }
            Ok(())
        })
        .resolve::<LogErrorAndDefault>();
}
