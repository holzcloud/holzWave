//! A probe binary: prints whether Apple's on-device language model is available and whether a Swift task can call
//! back into Rust. It is linked with the arguments the app's build script will pass, which makes it the file to copy to
//! another Mac (`scripts/apple-ai-launch-check.sh` builds, signs and runs it).
//!
//! Output, two lines: `availability: <key>` and `selftest: <ok|failed|skipped>`. Exit status 1 only when the self test
//! failed. Off macOS it prints `availability: unsupported_platform` and `selftest: skipped`.

use std::process::ExitCode;
use std::time::Duration;

use holzwave_apple_ai::{SELFTEST_VALUE, probe, selftest};

fn main() -> ExitCode {
    let availability = probe();
    println!("availability: {}", availability.as_str());
    if cfg!(not(target_os = "macos")) {
        println!("selftest: skipped");
        return ExitCode::SUCCESS;
    }
    if selftest(Duration::from_secs(5)) == Some(SELFTEST_VALUE) {
        println!("selftest: ok");
        ExitCode::SUCCESS
    } else {
        println!("selftest: failed");
        ExitCode::FAILURE
    }
}
