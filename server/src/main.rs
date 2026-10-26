#![deny(unsafe_code)]
//! The binary of the server: hands the command line to the library.

fn main() -> std::process::ExitCode {
    holzwave_server::cli::main(std::env::args().skip(1))
}
