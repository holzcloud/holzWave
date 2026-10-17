//! Asks the on-device model one question: the file to copy to another Mac.
//!
//! `chat [prompt]` prints the state of the model (`availability: <key>`), streams the reply of the prompt (default "Say
//! hello in one short sentence.") with control characters escaped, and ends with one status line.
//!
//! - `chat: ok` and exit status 0: the model answered.
//! - `chat: unavailable (<advice key>)` and exit status 0: the model cannot answer on this Mac; the key says what to do
//!   (`enable_apple_intelligence`, `wait_for_model`, `not_supported`, `update_macos`, `not_in_this_build`, `unknown`).
//! - `chat: failed (<error class>)` and exit status 1: the turn failed, for example `content_filter`.
//!
//! Off macOS it prints `availability: unsupported_platform` and `chat: unavailable (not_supported)`. It reads no
//! environment variable and writes no file.

use std::process::ExitCode;

use holzwave_apple_ai::probe;

const DEFAULT_PROMPT: &str = "Say hello in one short sentence.";

fn main() -> ExitCode {
    let prompt = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_PROMPT.to_owned());
    let state = probe();
    println!("availability: {}", state.as_str());
    if !state.is_available() {
        println!("chat: unavailable ({})", state.advice().as_str());
        return ExitCode::SUCCESS;
    }
    ask(&prompt)
}

#[cfg(not(target_os = "macos"))]
fn ask(_prompt: &str) -> ExitCode {
    println!("chat: unavailable (not_supported)");
    ExitCode::SUCCESS
}

/// The text with control characters and bidirectional overrides written out, so that a reply cannot send escape
/// sequences to the terminal. A line feed and a tab are kept.
#[cfg(target_os = "macos")]
fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let hostile = c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
        if hostile && c != '\n' && c != '\t' {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(target_os = "macos")]
fn ask(prompt: &str) -> ExitCode {
    use std::io::Write;

    use holzwave_apple_ai::{AppleProvider, MODEL_ID};
    use holzwave_core::ai::provider::{CancelToken, ChatRequest, Flow, Provider, StreamEvent};
    use holzwave_core::ai::types::Message;

    let provider = AppleProvider::system();
    let mut request = ChatRequest::new(MODEL_ID);
    request.system = "Answer briefly.".to_owned();
    request.messages.push(Message::user_text("chat-1", prompt, 0));
    let mut ended_with_newline = true;

    let result = provider.stream(
        &request,
        &mut |event| {
            if let StreamEvent::TextDelta(text) = event {
                let shown = escaped(&text);
                ended_with_newline = shown.ends_with('\n');
                print!("{shown}");
                let _ = std::io::stdout().flush();
            }
            Flow::Continue
        },
        &CancelToken::new(),
    );
    if !ended_with_newline {
        println!();
    }
    match result {
        Ok(_) => {
            println!("chat: ok");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!("chat: failed ({})", error.class().as_str());
            ExitCode::FAILURE
        }
    }
}
