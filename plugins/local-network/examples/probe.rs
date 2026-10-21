//! A probe binary for a person at a keyboard: prints whether this system has the Local Network privacy setting and what
//! one browse reads. Output, two lines: `applies: <bool>` and `reading: <allowed|denied|unknown>`.
//!
//! Run from Terminal (`cargo run -p holzwave-local-network --example probe`) the reading is `allowed` whatever the setting
//! is, because command-line tools started from Terminal are exempt from the privacy setting. Only an app bundle started
//! from Finder is subject to it. The probe is itself a local network operation: on a Mac where the app's permission is
//! undecided, the system shows its question.

use std::time::Duration;

use holzwave_local_network::{Reading, applies, probe};

fn main() {
    println!("applies: {}", applies());
    let word = match probe(Duration::from_millis(1500)) {
        Reading::Allowed => "allowed",
        Reading::Denied => "denied",
        Reading::Unknown => "unknown",
    };
    println!("reading: {word}");
}
