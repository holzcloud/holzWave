//! The command line of the server: the usage text and the choice of the subcommand.

use std::process::ExitCode;

/// What the server prints when it is called without a known subcommand.
pub const USAGE: &str = "usage: holzwave-server <subcommand>\n";

/// The exit status for a call the server does not understand.
const USAGE_STATUS: u8 = 2;

/// Runs the subcommand named by `args` (the arguments after the program name) and returns the exit status. A call that
/// names no subcommand, or one the server does not have, prints the usage and answers with the usage status.
fn run(args: &[String]) -> u8 {
    if let Some(name) = args.first() {
        eprintln!("holzwave-server: unknown subcommand {name:?}");
    }
    eprint!("{USAGE}");
    USAGE_STATUS
}

/// The entry point of the binary: `args` are the arguments after the program name.
pub fn main(args: impl IntoIterator<Item = String>) -> ExitCode {
    let args: Vec<String> = args.into_iter().collect();
    ExitCode::from(run(&args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_subcommand_and_no_subcommand_answer_with_the_usage_status() {
        assert_eq!(run(&["frobnicate".to_owned()]), USAGE_STATUS);
        assert_eq!(run(&[]), USAGE_STATUS);
    }
}
