#![deny(unsafe_code)]
//! The privileged helper of the holzWave server: the only process that holds file capabilities (Linux only).

fn main() -> std::process::ExitCode {
    eprintln!("usage: holzwave-spawn-helper (started by the holzwave-server supervisor on Linux only)");
    std::process::ExitCode::from(2)
}
