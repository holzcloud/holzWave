//! The canary set of the release gate: secrets that are built at run time, never written whole.
//!
//! Every secret is joined from a prefix and a pseudo-random body, so no line of this file is a secret-shaped literal
//! that a scanner or a reader could mistake for a leak. Each canary says which rule of the gate must fire on it. The
//! functions here are test code only.

use crate::common::Xorshift64;

/// One canary: a secret shaped the way a rule of the gate documents.
#[derive(Clone)]
pub struct Canary {
    /// The name of the canary; failing assertions print this and nothing else.
    pub name: &'static str,
    /// The rule of the gate that must fire on the canary.
    pub rule: &'static str,
    /// What a terminal prints (it may hold several lines, or an escape sequence inside the secret).
    pub line: String,
    /// The contiguous secret. A request that holds it has leaked it.
    pub needle: String,
    /// Pieces of 12 bytes or more of the secret. A request that holds one has leaked part of it.
    pub halves: Vec<String>,
}

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const URLSAFE: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
const BASE64: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const UPPER_DIGITS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const DIGITS: &str = "0123456789";

/// A fixed pseudo-random text of `length` characters from `alphabet`.
fn body(seed: u64, length: usize, alphabet: &str) -> String {
    let mut rng = Xorshift64::new(seed);
    let letters: Vec<char> = alphabet.chars().collect();
    (0..length).map(|_| letters[rng.below(letters.len())]).collect()
}

/// The parts joined: the one way a secret is assembled in this file.
fn join(parts: &[&str]) -> String {
    parts.concat()
}

/// The control character with this code point, made at run time.
fn control(code: u32) -> char {
    char::from_u32(code).expect("a valid code point")
}

/// The first and the last 12 characters of `text`.
fn ends(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    vec![chars[..12].iter().collect(), chars[chars.len() - 12..].iter().collect()]
}

/// Standard base64 of `bytes`, with padding.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a = u32::from(chunk[0]);
        let b = u32::from(chunk.get(1).copied().unwrap_or(0));
        let c = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (a << 16) | (b << 8) | c;
        out.push(char::from(TABLE[((n >> 18) & 63) as usize]));
        out.push(char::from(TABLE[((n >> 12) & 63) as usize]));
        out.push(if chunk.len() > 1 { char::from(TABLE[((n >> 6) & 63) as usize]) } else { '=' });
        out.push(if chunk.len() > 2 { char::from(TABLE[(n & 63) as usize]) } else { '=' });
    }
    out
}

/// A canary whose needle is the whole secret and whose line is `before`, the secret and `after`.
fn simple(name: &'static str, rule: &'static str, before: &str, needle: String, after: &str) -> Canary {
    Canary { name, rule, line: join(&[before, &needle, after]), needle, halves: Vec::new() }
}

/// The 17 canaries: one for each of the 14 secret rules of the gate and three ways to hide a secret from a pattern.
pub fn canaries() -> Vec<Canary> {
    let anthropic = join(&["sk-ant-", "api03-", &body(11, 56, URLSAFE)]);
    let pem_lines: Vec<String> = (0..4).map(|n| body(100 + n, 64, BASE64)).collect();
    let pem = Canary {
        name: "private_key",
        rule: "private_key",
        line: join(&[
            &join(&["-----BEGIN ", "OPENSSH", " PRIVATE KEY-----"]),
            "\n",
            &pem_lines.join("\n"),
            "\n",
            &join(&["-----END ", "OPENSSH", " PRIVATE KEY-----"]),
        ]),
        needle: pem_lines.join("\n"),
        halves: pem_lines.clone(),
    };

    let jwt = join(&[
        &join(&["eyJ", &body(21, 30, URLSAFE)]),
        ".",
        &join(&["eyJ", &body(22, 40, URLSAFE)]),
        ".",
        &body(23, 43, URLSAFE),
    ]);
    let authorization_value = body(31, 28, BASE64);
    let cookie_value = body(32, 32, ALNUM);

    // The escape-split key: a colour reset in the middle of the body.
    let split_body = body(41, 52, URLSAFE);
    let (first, second) = split_body.split_at(26);
    let escape_split = Canary {
        name: "escape_split",
        rule: "anthropic_key",
        line: join(&["export ANTHROPIC_API_KEY=sk-ant-", "api03-", first, &control(0x1b).to_string(), "[0m", second]),
        needle: join(&["sk-ant-", "api03-", &split_body]),
        halves: ends(&split_body),
    };

    // The zero-width split token: U+200B in the middle of the body.
    let token_body = body(51, 40, ALNUM);
    let (head, tail) = token_body.split_at(20);
    let zero_width_split = Canary {
        name: "zero_width_split",
        rule: "github_token",
        line: join(&["pushed with ghp_", head, &control(0x200b).to_string(), tail]),
        needle: join(&["ghp_", &token_body]),
        halves: ends(&token_body),
    };

    // The base64 form of a key: the key itself must not appear and neither must its base64 text.
    let hidden_key_body = body(61, 52, URLSAFE);
    let hidden_key = join(&["sk-ant-", "api03-", &hidden_key_body]);
    let encoded = base64(hidden_key.as_bytes());
    let base64_of_key = Canary {
        name: "base64_of_key",
        rule: "high_entropy",
        line: join(&["blob: ", &encoded]),
        needle: encoded,
        halves: ends(&hidden_key_body),
    };

    vec![
        pem,
        simple("anthropic_key", "anthropic_key", "export ANTHROPIC_API_KEY=", anthropic, ""),
        simple("openai_key", "openai_key", "provider ping ok using ", join(&["sk-", "proj-", &body(12, 48, URLSAFE)]), ""),
        simple("google_api_key", "google_api_key", "maps client ", join(&["AI", "za", &body(13, 35, URLSAFE)]), ""),
        simple("github_token", "github_token", "pushed with ", join(&["gh", "o_", &body(14, 38, ALNUM)]), ""),
        simple("aws_access_key", "aws_access_key", "aws principal ", join(&["AK", "IA", &body(15, 16, UPPER_DIGITS)]), " ok"),
        simple(
            "slack_token",
            "slack_token",
            "slack notify ",
            join(&["xo", "xb-", &body(16, 12, DIGITS), "-", &body(17, 12, DIGITS), "-", &body(18, 24, ALNUM)]),
            "",
        ),
        simple("jwt", "jwt", "session ", jwt, ""),
        Canary {
            name: "authorization",
            rule: "authorization",
            line: join(&["Authorization: Basic ", &authorization_value]),
            needle: authorization_value,
            halves: Vec::new(),
        },
        simple("bearer_token", "bearer_token", "retry with Bearer ", body(33, 34, ALNUM), ""),
        Canary {
            name: "cookie",
            rule: "cookie",
            line: join(&["Set-Cookie: sid=", &cookie_value, "; Path=/; HttpOnly"]),
            needle: cookie_value,
            halves: Vec::new(),
        },
        simple(
            "url_credentials",
            "url_credentials",
            "remote origin https://deploy:",
            body(34, 26, ALNUM),
            "@git.example.net/team/repo.git",
        ),
        simple("secret_assignment", "secret_assignment", "DB_PASSWORD=", body(35, 24, ALNUM), ""),
        simple("high_entropy", "high_entropy", "build id ", body(36, 56, ALNUM), ""),
        escape_split,
        zero_width_split,
        base64_of_key,
    ]
}

/// A marker sentence that no rule of the gate touches, distinctive for `tag`.
pub fn marker(tag: &str) -> String {
    format!("zq marker {tag} lantern orchard {}", body(70, 6, DIGITS))
}

/// The page of terminal text that holds every canary, each on its own line between ordinary shell output.
///
/// It is about 40 lines and several KiB. The marker `marker("screen")` is on one line so a test can tell the screen
/// text from the typed text.
pub fn terminal_text(set: &[Canary]) -> String {
    let ordinary = [
        "user@host ~/work/holz % ls -la",
        "total 48",
        "drwxr-xr-x  12 user  staff   384 Oct 17 09:12 .",
        "drwxr-xr-x   9 user  staff   288 Oct 16 18:40 ..",
        "-rw-r--r--   1 user  staff  1204 Oct 17 09:12 Cargo.toml",
        "drwxr-xr-x   6 user  staff   192 Oct 17 09:10 core",
        "drwxr-xr-x   4 user  staff   128 Oct 15 14:02 docs",
        "user@host ~/work/holz % cargo build --release",
        "   Compiling holzwave-core v0.1.0 (/work/holz/core)",
        "   Compiling holzwave-ipc v0.1.0 (/work/holz/ipc)",
        "   Compiling wsh v0.1.0 (/work/holz/wsh)",
        "    Finished release [optimized] target(s) in 41.20s",
        "user@host ~/work/holz % git status --short",
        " M core/src/ai/gate.rs",
        " M core/tests/ai_gate.rs",
        "?? notes.txt",
        "user@host ~/work/holz % git log --oneline -4",
        "e5dd13d docs: what the history does on other hosts",
        "9b7f0d6 feat: history entries carry the host of the pane",
        "fd3d020 fix: the upload rate is drawn in the secondary ink",
        "e7124a3 docs: the install lines of the opt-in binding",
        "user@host ~/work/holz % uname -a",
        "Darwin host 25.6.0 Darwin Kernel Version 25.6.0 arm64",
        "user@host ~/work/holz % df -h .",
        "Filesystem      Size    Used   Avail Capacity  Mounted on",
        "/dev/disk3s5   460Gi   301Gi   142Gi    68%    /System/Volumes/Data",
        "user@host ~/work/holz % echo done",
        "done",
    ];
    let mut lines: Vec<String> = Vec::new();
    for (index, line) in ordinary.iter().enumerate() {
        lines.push((*line).to_owned());
        if index == 6 {
            lines.push(marker("screen"));
        }
        if let Some(canary) = set.get(index / 2).filter(|_| index % 2 == 1) {
            lines.push(canary.line.clone());
        }
    }
    // Every canary that did not find its place above goes at the end, after a prompt.
    for canary in set.iter().skip(ordinary.len() / 2) {
        lines.push(canary.line.clone());
    }
    lines.push("user@host ~/work/holz % ".to_owned());
    let mut text = lines.join("\n");
    // Pad with ordinary build output so the page is several KiB, as a real screen of scrollback is.
    let filler: Vec<String> = (0..40)
        .map(|n| format!("   Compiling dependency-{n} v1.{n}.0 (/registry/src/index/dependency-{n}-1.{n}.0)"))
        .collect();
    text = format!("{}\n{text}", filler.join("\n"));
    text
}

/// The names of the canaries whose needle, or a piece of 12 bytes or more of it, occurs in `haystack`.
pub fn leaks(haystack: &[u8], set: &[Canary]) -> Vec<&'static str> {
    fn occurs(haystack: &[u8], needle: &str) -> bool {
        let needle = needle.as_bytes();
        !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
    }
    set.iter()
        .filter(|canary| {
            occurs(haystack, &canary.needle)
                || canary.halves.iter().filter(|half| half.len() >= 12).any(|half| occurs(haystack, half))
        })
        .map(|canary| canary.name)
        .collect()
}

/// The API key of the main profile of the rigs. No shape of the canary set matches it.
pub fn api_key() -> String {
    join(&["zq", "-api-", &body(81, 32, ALNUM)])
}

/// The key of one wire: the key canary and the wire tag.
pub fn wire_key(wire: &str) -> String {
    format!("{}-{wire}", api_key())
}

/// A value an environment variable with a secret word in its name holds.
pub fn env_value() -> String {
    join(&["zq", "-env-", &body(82, 30, ALNUM)])
}

/// A secret a command prints on its standard output.
pub fn output_value() -> String {
    join(&["gh", "o_", &body(83, 38, ALNUM)])
}

/// A secret that sits in a file the tools read.
pub fn file_value() -> String {
    body(84, 26, ALNUM)
}
