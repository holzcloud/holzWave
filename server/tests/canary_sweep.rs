//! The canary sweep over the real server process.
//!
//! The server under test is the product's own binary, started as a process. The binary is built with the `in-process-workers`
//! feature, so a person's shell runs inside the server's own process; the path through the spawn helper (the product mode on
//! Linux) belongs to the isolation and image tests and is not covered here.
//!
//! A canary is built at run time. The sweep searches every surface the server leaves behind (the data volume with the database
//! and its write-ahead log, the captured standard output and standard error, every response and WebSocket frame the test saw,
//! the process's environment and argument list, and the `Debug` text of key-bearing types) in every form a program could have
//! written it, and a failure names the surface, the canary and the form, never the text.
#![cfg(feature = "in-process-workers")]

#[path = "support/edge.rs"]
mod edge;
#[path = "support/edge_tls.rs"]
mod edge_tls;
#[path = "support/real_process.rs"]
mod real_process;
#[path = "support/sweep.rs"]
mod sweep;
#[path = "support/unlock_env.rs"]
mod unlock_env;
#[path = "support/ws_env.rs"]
mod ws_env;

use std::path::PathBuf;
use std::time::Duration;

use real_process::{Exchange, Server, Site};
use serde_json::json;
use sweep::{Canary, Category, Counts, Finding, Sweep};

// ---- the sweep itself: pure tests --------------------------------------------------------------------------------------------

fn secret(name: &str) -> Canary {
    Canary::new(name, Category::Secret)
}

#[test]
fn a_canary_has_the_prefix_and_24_random_hex_characters_and_two_canaries_differ() {
    let a = secret("a");
    let b = secret("b");
    assert!(a.text().starts_with(sweep::PREFIX));
    assert_eq!(a.text().len(), sweep::PREFIX.len() + sweep::RANDOM_HEX);
    assert!(a.text()[sweep::PREFIX.len()..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
    assert_ne!(a.text(), b.text());
}

/// The forms of one canary, by name.
fn form_names(canary: &Canary) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = canary.forms().into_iter().map(|(n, _)| n).collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[test]
fn every_form_is_searched_and_every_form_is_found_inside_a_longer_buffer() {
    let canary = secret("probe");
    let sweep = Sweep::new(&[canary.clone()]);
    let names = form_names(&canary);
    for wanted in [
        "plain", "base64", "hex-lower", "hex-upper", "percent-all", "json-unicode", "base32", "base32-lower", "reversed",
        "utf16le", "utf16be",
    ] {
        assert!(names.contains(&wanted), "the form {wanted} is searched: {names:?}");
    }
    // The URL alphabet differs from the standard one only in two characters; when the canary's bytes do not produce them the
    // two searches are the same bytes and run once.
    for (name, form) in canary.forms() {
        let mut buffer = b"\x00\xffsome bytes before ".to_vec();
        buffer.extend_from_slice(&form);
        buffer.extend_from_slice(b" and bytes after\x00\x01");
        let found = sweep.scan_bytes("buffer", &buffer);
        assert!(found.iter().any(|f| f.form == name), "form {name} is found inside a binary buffer: {found:?}");
        assert!(found.iter().all(|f| f.canary == "probe" && f.surface == "buffer"));
    }
}

#[test]
fn the_three_alignments_of_base64_are_found_when_the_canary_sits_at_any_offset_of_a_longer_text() {
    use base64::Engine as _;
    let canary = secret("probe");
    let sweep = Sweep::new(&[canary.clone()]);
    for lead in 0..6usize {
        let mut raw = vec![b'x'; lead];
        raw.extend_from_slice(canary.text().as_bytes());
        raw.extend_from_slice(b"tail");
        for (label, text) in [
            ("standard", base64::engine::general_purpose::STANDARD.encode(&raw)),
            ("url", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&raw)),
        ] {
            let found = sweep.scan_text("text", &text);
            assert!(!found.is_empty(), "base64 ({label}) of the canary at offset {lead} is found");
        }
        let five = sweep::base32(&raw);
        assert!(!sweep.scan_text("text", &five).is_empty(), "base32 of the canary at offset {lead} is found");
    }
}

#[test]
fn a_buffer_without_the_canary_and_a_cut_canary_are_a_miss() {
    let canary = secret("probe");
    let sweep = Sweep::new(&[canary.clone()]);
    assert!(sweep.scan_bytes("empty", b"").is_empty());
    assert!(sweep.scan_bytes("noise", &vec![0xa5; 4096]).is_empty());
    let cut = &canary.text()[..canary.text().len() - 1];
    assert!(sweep.scan_text("cut", cut).is_empty(), "one character short is not the canary");
    let other = secret("other");
    assert!(sweep.scan_text("other", other.text()).is_empty(), "another canary is not this one");
}

#[test]
fn a_finding_names_the_surface_the_canary_and_the_form_and_never_the_text() {
    let canary = secret("probe");
    let sweep = Sweep::new(&[canary.clone()]);
    let found = sweep.scan_text("somewhere", &format!("before {} after", canary.text()));
    assert_eq!(found.len(), 1);
    let line = found[0].line();
    assert!(line.contains("somewhere") && line.contains("probe") && line.contains("plain"));
    assert!(!line.contains(canary.text()) && !format!("{canary:?}").contains(canary.text()));
}

#[test]
fn the_content_filter_lets_a_canary_through_in_its_owners_home_and_nowhere_else() {
    let owned = Canary::new("scroll", Category::Content { owner: "alice".to_owned() });
    let sweep = Sweep::new(&[owned.clone(), secret("pw")]);
    let home = PathBuf::from("/data/homes/20000");
    let found = vec![
        Finding { surface: "/data/homes/20000/log".to_owned(), canary: "scroll".to_owned(), form: "plain", offset: 0 },
        Finding { surface: "/data/server.db".to_owned(), canary: "scroll".to_owned(), form: "plain", offset: 0 },
        Finding { surface: "/data/homes/20000/log".to_owned(), canary: "pw".to_owned(), form: "plain", offset: 0 },
    ];
    let bad = sweep::outside_home(found, &sweep, &[("alice".to_owned(), home)]);
    let names: Vec<String> = bad.iter().map(|f| format!("{}@{}", f.canary, f.surface)).collect();
    assert_eq!(names, ["scroll@/data/server.db", "pw@/data/homes/20000/log"], "a secret is never allowed in a home");
}

// ---- the negative control ----------------------------------------------------------------------------------------------------

/// A secret canary planted in a file of a scratch server directory, in one standard-error line and in one response body (and
/// an encoded one at an odd alignment inside a longer buffer): the sweep must report each with its surface and form.
fn plant_and_sweep() -> Vec<Finding> {
    let planted = secret("planted");
    let encoded = secret("encoded");
    let sweep = Sweep::new(&[planted.clone(), encoded.clone()]);
    let scratch = ws_env::scratch::Scratch::new("sweep-control");
    let server_dir = scratch.join("server");
    std::fs::create_dir(&server_dir).expect("a server directory");
    scratch.write_file("server/state.db", format!("header\0{}\0trailer", planted.text()).as_bytes(), 0o600);
    let mut counts = Counts::default();
    let mut found = sweep.scan_tree(&server_dir, &mut counts);
    found.extend(sweep.scan_lines("stderr", &["fine".to_owned(), format!("failed for {}", planted.text())], &mut counts));
    let reply = edge_tls::Reply { status: 200, headers: vec![("x".to_owned(), "y".to_owned())], body: format!("{{\"v\":\"{}\"}}", planted.text()).into_bytes() };
    found.extend(sweep.scan_bytes("response body", &reply.body));
    // The base64 of the canary at an odd alignment inside a longer buffer.
    use base64::Engine as _;
    let mut raw = b"a".to_vec();
    raw.extend_from_slice(encoded.text().as_bytes());
    raw.extend_from_slice(b"zz");
    let text = format!("prefix {} suffix", base64::engine::general_purpose::STANDARD.encode(&raw));
    found.extend(sweep.scan_text("encoded buffer", &text));
    found
}

#[test]
fn negative_control_a_planted_canary_is_found_on_every_surface_and_in_an_encoded_form() {
    let found = plant_and_sweep();
    let report: Vec<String> = found.iter().map(|f| format!("{} / {} / {}", f.surface_kind(), f.canary, f.form)).collect();
    println!("negative control: {report:#?}");
    assert!(found.iter().any(|f| f.canary == "planted" && f.surface.ends_with("state.db") && f.form == "plain"), "the file: {report:?}");
    assert!(found.iter().any(|f| f.canary == "planted" && f.surface == "stderr" && f.form == "plain"), "the line: {report:?}");
    assert!(found.iter().any(|f| f.canary == "planted" && f.surface == "response body" && f.form == "plain"), "the body: {report:?}");
    assert!(found.iter().any(|f| f.canary == "encoded" && f.surface == "encoded buffer" && f.form == "base64"), "the encoded form: {report:?}");
}

trait SurfaceKind {
    fn surface_kind(&self) -> &str;
}

impl SurfaceKind for Finding {
    /// The surface without the scratch directory's random name.
    fn surface_kind(&self) -> &str {
        self.surface.rsplit('/').next().unwrap_or(&self.surface)
    }
}

// ---- the real process: the tracer --------------------------------------------------------------------------------------------

fn exchange_surface(index: usize, exchange: &Exchange, part: &str) -> String {
    format!("response #{index} {} {} {part}", exchange.method, exchange.target.split('?').next().unwrap_or_default())
}

/// Scans the data volume, the captured output, every response and frame, the environment and the arguments.
fn scan_all(server: &Server, sweep: &Sweep, counts: &mut Counts) -> Vec<Finding> {
    let mut found = Vec::new();
    found.extend(sweep.scan_tree(&server.site.data_dir(), counts));
    found.extend(sweep.scan_tree(&server.site.run_dir(), counts));
    found.extend(sweep.scan_lines("stdout", &server.stdout_lines(), counts));
    found.extend(sweep.scan_lines("stderr", &server.stderr_lines(), counts));
    for (index, exchange) in server.recorder.exchanges().iter().enumerate() {
        counts.bytes += exchange.body.len();
        found.extend(sweep.scan_bytes(&exchange_surface(index, exchange, "body"), &exchange.body));
        let headers: String = exchange.headers.iter().map(|(n, v)| format!("{n}: {v}\n")).collect();
        found.extend(sweep.scan_text(&exchange_surface(index, exchange, "headers"), &headers));
    }
    for (index, frame) in server.recorder.frames().iter().enumerate() {
        counts.lines += 1;
        found.extend(sweep.scan_text(&format!("frame #{index}"), frame));
    }
    let (environment, arguments) = server.environment_and_arguments();
    found.extend(sweep.scan_bytes("environment", &environment));
    found.extend(sweep.scan_bytes("arguments", &arguments));
    found
}

/// The names of the exchanges (target and number) where a canary was found.
fn places(found: &[Finding], canary: &str) -> Vec<String> {
    let mut places: Vec<String> = found.iter().filter(|f| f.canary == canary).map(|f| f.surface.clone()).collect();
    places.sort();
    places.dedup();
    places
}

#[tokio::test(flavor = "multi_thread")]
async fn tracer_the_real_binary_serves_a_person_who_uses_a_shell_and_files_and_no_surface_holds_a_secret() {
    // The canaries. The client secret is read by the server from a file the test writes outside the server directory.
    let client_secret = secret("oidc client secret");
    let password = secret("password");
    let command = Canary::new("command", Category::Content { owner: "alice".to_owned() });
    let scroll = Canary::new("scrollback", Category::Content { owner: "alice".to_owned() });
    let uploaded = Canary::new("upload", Category::Content { owner: "alice".to_owned() });
    let assistant_key = secret("assistant key");

    let site = Site::new(Some(client_secret.text()));
    let mut server = site.start();
    println!("Process: healthy after {:?}; configuration keys: public_url, listen, data_dir, runtime_dir, tls.{{mode,cert_file,key_file}}, auth.{{passkey,password_totp,oidc[1]}}, workers.{{mode,uid_min,uid_max}}", server.started.elapsed());
    let (code, word) = server.healthcheck();
    assert_eq!((code, word.as_str()), (Some(0), "healthy"), "the binary's own healthcheck agrees");

    // The setup token is read from standard output only, and used once.
    let token = server.setup_token();
    let setup = Canary::with_text_of("setup token", &token);
    let mut client = server.client();
    let session = client.session().await;
    assert_eq!(session["bootstrap"], json!(true), "a valid start has no account");
    let enrolled = client.enrol("/auth/bootstrap", &token, "alice", password.text()).await;
    let totp = Canary::with_text_of("authenticator secret", &enrolled.secret);
    let recovery = Canary::with_text_of("recovery code", &enrolled.recovery);
    let second_use = client.call("POST", "/auth/bootstrap", Some(&json!({"step": "begin", "token": token, "username": "mallory", "display_name": "m", "method": "password", "password": "another correct horse"}))).await;
    assert_eq!(second_use.status, 410, "the setup token works once: {}", second_use.text());

    // A shell: the output is built from two fragments, so the typed text and the output differ.
    let mut socket = client.socket().await;
    let pane = socket.start_focused_pane().await;
    let (first, second) = scroll.text().split_at(14);
    socket.type_line(pane, &format!("A={first}; B={second}; printf '%s\\n' \"$A$B\"; : {}", command.text())).await;
    let seen = socket.event_where("frame", Duration::from_secs(20), |payload| payload.to_string().contains(scroll.text())).await;
    assert!(seen.is_some(), "the output of the shell comes back in a frame");

    // A secret through the real commands.
    let profile = socket.ok("ai_profile_save", &json!({"input": {"name": "Cloud", "preset": "anthropic"}})).await.as_str().expect("an id").to_owned();
    let stored = socket.call("ai_key_set", &json!({"profile": profile, "slot": "provider", "api_key": assistant_key.text()})).await;
    let secret_store_open = stored.get("ok").is_some();
    println!("Tracer: the assistant key command answered {}", if secret_store_open { "ok" } else { "an error (store not open)" });

    // A file: upload, grant, download.
    // The file is a picture to the product (a PNG signature and then the canary), because only media can be granted a link.
    let mut put = client.page_request("PUT", "/api/upload?name=canary.png", None).header("Content-Type", "application/octet-stream");
    put.body = [&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A][..], uploaded.text().as_bytes()].concat();
    let answer = client.send(put).await;
    assert_eq!(answer.status, 200, "upload: {}", answer.text());
    let path = answer.json()["path"].as_str().expect("the path of the upload").to_owned();
    let grant = socket.ok("file_grant", &json!({"source": "local", "path": path})).await;
    let url = grant["url"].as_str().expect("a download address").to_owned();
    let download = client.call("GET", &url, None).await;
    assert_eq!(download.status, 200);
    assert!(download.body.windows(uploaded.text().len()).any(|w| w == uploaded.text().as_bytes()), "the download holds the upload");

    // ---- the sweep, while the server runs (the write-ahead log and the shared-memory file exist) ----
    let all = [client_secret.clone(), password.clone(), command.clone(), scroll.clone(), uploaded.clone(), assistant_key.clone(), setup.clone(), totp.clone(), recovery.clone()];
    let sweep = Sweep::new(&all);
        let mut counts = Counts::default();
    let running = scan_all(&server, &sweep, &mut counts);
    let wal_seen = sweep::files_below(&server.site.data_dir()).iter().any(|p| p.to_string_lossy().ends_with("-wal") || p.to_string_lossy().ends_with("-shm"));
    println!("Tracer (running): {} files, {} bytes, {} lines/frames; write-ahead log or shared memory present: {wal_seen}", counts.files, counts.bytes, counts.lines);

    // ---- stop cleanly and sweep again ----
    assert_eq!(server.stop(), Some(0), "SIGTERM ends the process with 0");
    assert!(!server.site.run_dir().join("health.sock").exists());
    let mut counts_stopped = Counts::default();
    let stopped = scan_all(&server, &sweep, &mut counts_stopped);
    println!("Tracer (stopped): {} files, {} bytes, {} lines/frames", counts_stopped.files, counts_stopped.bytes, counts_stopped.lines);

    let homes: Vec<(String, PathBuf)> = std::fs::read_dir(server.homes())
        .map(|d| d.flatten().map(|e| ("alice".to_owned(), e.path())).collect())
        .unwrap_or_default();
    assert_eq!(homes.len(), 1, "one person has one home");
    for (when, found) in [("running", running), ("stopped", stopped)] {
        // A secret is found only where the product shows it to its owner.
        let secrets: Vec<Finding> = found.iter().filter(|f| sweep.canaries().iter().any(|c| c.name == f.canary && c.is_secret())).cloned().collect();
        for name in ["oidc client secret", "password", "assistant key"] {
            assert!(places(&secrets, name).is_empty(), "{when}: {name} is found nowhere: {:?}", places(&secrets, name));
        }
        let totp_places = places(&secrets, "authenticator secret");
        assert!(totp_places.iter().all(|p| p.contains("POST /auth/bootstrap") && p.ends_with("body")), "{when}: the authenticator secret only in its answer: {totp_places:?}");
        assert_eq!(totp_places.len(), 1, "{when}: in one answer: {totp_places:?}");
        let recovery_places = places(&secrets, "recovery code");
        assert!(recovery_places.iter().all(|p| p.contains("POST /auth/bootstrap") && p.ends_with("body")), "{when}: the recovery code only in its answer: {recovery_places:?}");
        assert_eq!(recovery_places.len(), 1, "{when}: in one answer: {recovery_places:?}");
        assert_eq!(places(&secrets, "setup token"), ["stdout"], "{when}: the setup token is the one standard-output line");
        // A content canary is inside its owner's home, in the owner's own frames and responses, and nowhere else.
        let contents: Vec<Finding> = found.iter().filter(|f| sweep.canaries().iter().any(|c| c.name == f.canary && !c.is_secret())).cloned().collect();
        let owner_view: Vec<Finding> = contents
            .into_iter()
            .filter(|f| !(f.surface.starts_with("frame #") || f.surface.starts_with("response #")))
            .collect();
        let outside = sweep::outside_home(owner_view, &sweep, &homes);
        assert!(outside.is_empty(), "{when}: content canaries outside the owner's home: {:?}", outside.iter().map(Finding::line).collect::<Vec<_>>());
    }

    // The scanner of the unlock plumbing's suite covers the volume as well; its answer must agree.
    let needles: Vec<(&str, &str)> = [(&client_secret, "oidc client secret"), (&password, "password"), (&assistant_key, "assistant key")]
        .iter()
        .map(|(c, n)| (*n, c.text()))
        .collect();
    let (hits, read) = unlock_env::scan_volume_for(&[server.site.data_dir(), server.site.run_dir()], &needles);
    assert!(hits.is_empty(), "the unlock suite's scanner finds nothing: {hits:?}");
    println!("Tracer: the unlock plumbing's scanner read {read} files and found nothing");

    // The sweep must have seen what it should see: the scrollback and the upload are in the owner's own frames and responses.
    let again = scan_all(&server, &sweep, &mut Counts::default());
    assert!(again.iter().any(|f| f.canary == "scrollback" && f.surface.starts_with("frame #")), "the sweep sees the scrollback in the owner's frames");
    assert!(again.iter().any(|f| f.canary == "upload" && f.surface.contains("GET /files/")), "the sweep sees the upload in the owner's download");
    assert!(again.iter().any(|f| f.canary == "setup token" && f.surface == "stdout"), "the sweep sees the setup token on standard output");
    let upload_places = places(&again, "upload");
    println!("Tracer: hits and their places: upload at {upload_places:?}; scrollback in {} frames", again.iter().filter(|f| f.canary == "scrollback" && f.surface.starts_with("frame #")).count());
    println!("Forms: {:?}", form_names(&password));
}

impl Canary {
    /// A canary for a value the product made (a token, a code, a secret), built after the fact.
    fn with_text_of(name: &str, text: &str) -> Canary {
        Canary::with_text(name, Category::Secret, text)
    }
}
