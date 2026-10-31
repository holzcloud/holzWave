//! Support for the compression tests: the measurement's verdict file, an upgrade request that offers the extension, and a
//! configuration check that runs on a text and a scratch key. The server itself is started through `ws_env`.
//!
//! The test file that includes this one also includes `ws_env` (which owns the scratch directories) and `edge_tls`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use base64::Engine;
use holzwave_server::config::{self, Env, Prepared, Probe, RealProbe, Refusal};
use holzwave_server::keyfile::ServerKey;
use serde_json::{Value, json};

use crate::edge_tls::Request;
use crate::ws_env::WsEnv;
use crate::ws_env::scratch::Scratch;

/// The verdict file of the measurement, next to the measurement's other results.
pub fn verdict_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../spike/web-deflate/VERDICT")
}

/// Every line of the verdict file.
pub fn verdict_lines() -> Vec<String> {
    let text = std::fs::read_to_string(verdict_path()).expect("the verdict file of the measurement exists");
    text.lines().map(str::to_owned).collect()
}

/// The first line of the verdict file, which is `pass` or `fail` and nothing else.
pub fn verdict() -> String {
    let first = verdict_lines().into_iter().next().expect("the verdict file is not empty");
    assert!(first == "pass" || first == "fail", "the first line of the verdict file is pass or fail, not {first:?}");
    first
}

/// The upgrade request of a browser that offers the extension, as a browser words it.
pub fn offering(cookie: &str, origin: &str, offer: &str) -> Request {
    WsEnv::upgrade_request(Some(cookie), Some(origin), None).header("Sec-WebSocket-Extensions", offer)
}

/// The values of the header `name` of a handshake answer.
pub fn answer_values<'a>(handshake: &'a [(String, String)], name: &str) -> Vec<&'a str> {
    handshake.iter().filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect()
}

const CONFIG_PATH: &str = "/etc/test/server.json";

/// A probe that serves one configuration text and the key from a real scratch file.
struct TextProbe {
    config: String,
}

impl Probe for TextProbe {
    fn read_config(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        if path == Path::new(CONFIG_PATH) { Ok(self.config.clone().into_bytes()) } else { Err(std::io::ErrorKind::NotFound.into()) }
    }

    fn readable(&self, path: &Path) -> bool {
        ["/certs/cert.pem", "/certs/key.pem"].iter().any(|p| path == Path::new(p))
    }

    fn own_uid(&self) -> u32 {
        10_001
    }

    fn load_key(&self, path: &Path) -> Result<ServerKey, Refusal> {
        RealProbe.load_key(path)
    }
}

/// The smallest configuration that is accepted, with `transport` set when given.
pub fn minimal(transport: Option<Value>) -> Value {
    let mut file = json!({
        "public_url": "https://hw.example.test",
        "tls": {"mode": "files", "cert_file": "/certs/cert.pem", "key_file": "/certs/key.pem"},
        "auth": {"passkey": true}
    });
    if let Some(transport) = transport {
        file["transport"] = transport;
    }
    file
}

/// Runs the configuration check on `file` with a fresh key file.
pub fn prepare_text(file: &Value) -> Result<Prepared, Refusal> {
    let dir = Scratch::new("deflate");
    let line = format!("hws1:{}\n", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([4u8; 32]));
    let key = dir.write_file("server.key", line.as_bytes(), 0o600);
    let mut env = Env::new();
    env.insert("HOLZWAVE_SERVER_KEY_FILE".to_owned(), key.to_str().expect("utf-8 path").to_owned());
    env.insert("HOLZWAVE_SERVER_CONFIG".to_owned(), CONFIG_PATH.to_owned());
    config::prepare(&env, &TextProbe { config: file.to_string() })
}
