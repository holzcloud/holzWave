//! Accounts: the first administrator, invitations, the administrator actions and single sign-on accounts.
#![allow(dead_code)]

#[path = "support/scratch.rs"]
mod scratch;

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use holzwave_server::accounts::{AccountError, Accounts, NewAccount};
use holzwave_server::audit::Audit;
use holzwave_server::clock::{Clock, ManualClock, SystemClock};
use holzwave_server::store::{AccountState, Role, Store, StoreError};
use scratch::Scratch;
use serde_json::{Value, json};

/// 2026-03-01T08:30:00Z
const NOW: i64 = 1_772_353_800;
const RANGE: std::ops::RangeInclusive<u32> = 20000..=29999;
const ADDR: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));

/// A store, an audit log and a clock in a scratch directory, and the service over them.
struct Fixture {
    scratch: Scratch,
    store: Arc<Store>,
    clock: Arc<dyn Clock>,
    accounts: Accounts,
}

fn fixture_with(tag: &str, clock: Arc<dyn Clock>) -> Fixture {
    let scratch = Scratch::new(tag);
    let store = Arc::new(Store::open(scratch.path()).expect("open the store"));
    let audit = Arc::new(Audit::open(scratch.path(), clock.clone()).expect("open the audit log"));
    let accounts = Accounts::new(store.clone(), clock.clone(), audit, RANGE);
    Fixture { scratch, store, clock, accounts }
}

fn fixture(tag: &str) -> Fixture {
    fixture_with(tag, Arc::new(ManualClock::new(NOW)))
}

/// Every audit line of every day, in order, as JSON.
fn audit_lines(dir: &Path) -> Vec<Value> {
    let audit = dir.join("server/audit");
    let mut files: Vec<_> = match std::fs::read_dir(&audit) {
        Ok(entries) => entries.map(|e| e.expect("entry").path()).collect(),
        Err(_) => return Vec::new(),
    };
    files.sort();
    files
        .iter()
        .flat_map(|f| std::fs::read_to_string(f).expect("read an audit file").lines().map(str::to_owned).collect::<Vec<_>>())
        .map(|l| serde_json::from_str(&l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")))
        .collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn person<'a>(username: &'a str, display_name: &'a str) -> NewAccount<'a> {
    NewAccount { username, display_name, handle: None }
}

/// A configuration in the proxy TLS mode (no certificate files), a key file made by `keygen`, and the environment that names
/// them with the scratch directory as the data directory.
struct Installed {
    dir: Scratch,
    env: Vec<(&'static str, String)>,
}

fn installed(tag: &str) -> Installed {
    let dir = Scratch::new(tag);
    let config = json!({
        "public_url": "https://hw.example.test",
        "tls": {"mode": "proxy", "trusted_proxies": ["127.0.0.1/32"]},
        "auth": {"password_totp": true}
    });
    let config_path = dir.write_file("server.json", config.to_string().as_bytes(), 0o644);
    let key = dir.join("server.key");
    let made = Command::new(env!("CARGO_BIN_EXE_holzwave-server"))
        .args(["keygen", "--out", key.to_str().expect("utf-8 path")])
        .env_clear()
        .output()
        .expect("run keygen");
    assert_eq!(made.status.code(), Some(0), "keygen: {}", text(&made.stderr));
    let data = dir.join("data");
    std::fs::create_dir(&data).expect("data dir");
    let env = vec![
        ("HOLZWAVE_SERVER_CONFIG", config_path.to_str().expect("utf-8").to_owned()),
        ("HOLZWAVE_SERVER_KEY_FILE", key.to_str().expect("utf-8").to_owned()),
        ("HOLZWAVE_DATA_DIR", data.to_str().expect("utf-8").to_owned()),
    ];
    Installed { dir, env }
}

impl Installed {
    fn data(&self) -> std::path::PathBuf {
        self.dir.join("data")
    }

    fn bootstrap(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_holzwave-server"))
            .arg("bootstrap")
            .env_clear()
            .envs(self.env.iter().map(|(k, v)| (*k, v.as_str())))
            .output()
            .expect("run the server binary")
    }

    /// The service over the data directory the command used, on the system clock the command used.
    fn accounts(&self) -> (Arc<Store>, Accounts) {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let store = Arc::new(Store::open(&self.data()).expect("open the store"));
        let audit = Arc::new(Audit::open(&self.data(), clock.clone()).expect("open the audit log"));
        (store.clone(), Accounts::new(store, clock, audit, RANGE))
    }
}

/// The token in a line the command printed: `hwb_` and 43 characters of the base64url alphabet.
fn token_in(line: &str) -> String {
    let start = line.find("hwb_").expect("the line holds a bootstrap token");
    let token: String = line[start..].chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).collect();
    assert_eq!(token.len(), 4 + 43, "token length: {token}");
    token
}

// ---------------------------------------------------------------------------------------------------------------------
// The first administrator
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn tracer_bootstrap_command_prints_one_line_with_the_token() {
    let setup = installed("accounts-tracer-print");
    let out = setup.bootstrap();
    let stdout = text(&out.stdout);
    println!("Tracer: stdout = {}", stdout.replace(&token_in(&stdout), "<token>").trim_end());
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    assert_eq!(stdout.lines().count(), 1, "stdout: {stdout:?}");
    assert!(stdout.ends_with('\n'));
    assert!(stdout.contains("https://hw.example.test"), "{stdout}");
    let token = token_in(&stdout);
    assert!(!text(&out.stderr).contains(&token), "the token must not reach standard error");
    // Nothing secret is kept: the files of the data directory hold the hash, not the token.
    let (store, _) = setup.accounts();
    drop(store);
    for entry in walk(&setup.data()) {
        let bytes = std::fs::read(&entry).expect("read a file");
        assert!(!bytes.windows(token.len()).any(|w| w == token.as_bytes()), "token found in {}", entry.display());
    }
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn tracer_the_token_makes_the_first_administrator_once() {
    let setup = installed("accounts-tracer-redeem");
    let out = setup.bootstrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", text(&out.stderr));
    let token = token_in(&text(&out.stdout));
    let (store, accounts) = setup.accounts();

    let mut enrolled: Option<String> = None;
    let account = accounts
        .redeem_bootstrap(&token, &person("alice", "Alice"), Some(ADDR), |_, a| -> Result<(), StoreError> {
            enrolled = Some(a.username.clone());
            Ok(())
        })
        .expect("redeem");
    println!(
        "Tracer: account = id {} username {} role {} uid {} state {}",
        account.id,
        account.username,
        account.role.as_str(),
        account.uid,
        account.state.as_str()
    );
    assert_eq!(enrolled.as_deref(), Some("alice"));
    assert_eq!(account.role, Role::Admin);
    assert_eq!(account.uid, *RANGE.start());
    assert_eq!(account.state, AccountState::Active);
    assert_eq!(store.read(|tx| tx.account_count()).expect("count"), 1);

    let lines = audit_lines(&setup.data());
    println!("Tracer: audit = {}", lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n"));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["action"], "bootstrap.redeem");
    assert_eq!(lines[0]["result"], "ok");
    assert_eq!(lines[0]["target"], account.id.to_string());
    assert!(!lines[0].to_string().contains(&token));

    // The token is spent: the second use is the one error and writes a failed line.
    let again = accounts.redeem_bootstrap(&token, &person("mallory", "Mallory"), Some(ADDR), |_, _| -> Result<(), StoreError> {
        panic!("the enrolment must not run for a spent token")
    });
    assert_eq!(again.expect_err("spent"), AccountError::NotRedeemable);
    assert_eq!(store.read(|tx| tx.account_count()).expect("count"), 1);
    let lines = audit_lines(&setup.data());
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["action"], "bootstrap.redeem");
    assert_eq!(lines[1]["result"], "fail");
}

#[test]
fn tracer_the_command_refuses_once_an_account_exists() {
    let setup = installed("accounts-tracer-refuse");
    let out = setup.bootstrap();
    let token = token_in(&text(&out.stdout));
    let (_, accounts) = setup.accounts();
    accounts
        .redeem_bootstrap(&token, &person("alice", "Alice"), None, |_, _| -> Result<(), StoreError> { Ok(()) })
        .expect("redeem");
    drop(accounts);

    let out = setup.bootstrap();
    println!("Tracer: second run = status {:?}, stderr {}", out.status.code(), text(&out.stderr).trim_end());
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "stdout: {}", text(&out.stdout));
    assert!(text(&out.stderr).contains("first administrator"), "{}", text(&out.stderr));
    assert!(!text(&out.stderr).contains("hwb_"));
}

#[test]
fn tracer_a_new_token_replaces_the_old_one() {
    let setup = installed("accounts-tracer-replace");
    let first = token_in(&text(&setup.bootstrap().stdout));
    let second = token_in(&text(&setup.bootstrap().stdout));
    assert_ne!(first, second);
    let (_, accounts) = setup.accounts();
    let old = accounts.redeem_bootstrap(&first, &person("alice", "Alice"), None, |_, _| -> Result<(), StoreError> { Ok(()) });
    assert_eq!(old.expect_err("replaced"), AccountError::NotRedeemable);
    accounts
        .redeem_bootstrap(&second, &person("alice", "Alice"), None, |_, _| -> Result<(), StoreError> { Ok(()) })
        .expect("the newest token works");
}
