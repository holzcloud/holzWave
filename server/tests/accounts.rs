//! Accounts: the first administrator, invitations, the administrator actions and single sign-on accounts.

#[path = "support/scratch.rs"]
mod scratch;

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use holzwave_server::accounts::{AccountError, AccountObserver, Accounts, Ending, NewAccount};
use holzwave_server::audit::Audit;
use holzwave_server::clock::{Clock, ManualClock, SystemClock};
use holzwave_server::store::{Account, AccountId, AccountState, NewSession, Role, SignInMethod, Store, StoreError, Tx, hash_token};
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
    clock: Arc<ManualClock>,
    accounts: Accounts,
}

fn fixture_observed(tag: &str, observer: Option<Arc<dyn AccountObserver>>) -> Fixture {
    let scratch = Scratch::new(tag);
    let clock = Arc::new(ManualClock::new(NOW));
    let store = Arc::new(Store::open(scratch.path()).expect("open the store"));
    let audit = Arc::new(Audit::open(scratch.path(), clock.clone()).expect("open the audit log"));
    let mut accounts = Accounts::new(store.clone(), clock.clone(), audit, RANGE);
    if let Some(observer) = observer {
        accounts = accounts.with_observer(observer);
    }
    Fixture { scratch, store, clock, accounts }
}

fn fixture(tag: &str) -> Fixture {
    fixture_observed(tag, None)
}

/// An enrolment that stores nothing and succeeds.
fn ok(_: &Tx<'_>, _: &Account) -> Result<(), StoreError> {
    Ok(())
}

/// The first administrator, made through the bootstrap token.
fn first_admin(f: &Fixture) -> Account {
    let issued = f.accounts.bootstrap_issue().expect("issue");
    f.accounts.redeem_bootstrap(issued.token.expose(), &person("root", "Root"), None, ok).expect("redeem")
}

/// A member, made through an invitation of `admin`.
fn member(f: &Fixture, admin: &Account, name: &str) -> Account {
    let issued = f.accounts.invite_create(admin.id, None, None, Role::Member).expect("invite");
    f.accounts.redeem_invite(issued.token.expose(), &person(name, name), None, ok).expect("redeem")
}

fn account_of(f: &Fixture, id: AccountId) -> Account {
    f.store.read(|tx| tx.account_get(id)).expect("read").expect("the account")
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

// ---------------------------------------------------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn invite_create_stores_only_the_hash_and_seven_days() {
    use base64::Engine as _;
    let f = fixture("accounts-invite-hash");
    let admin = first_admin(&f);
    let issued = f.accounts.invite_create(admin.id, Some(ADDR), None, Role::Member).expect("invite");
    let token = issued.token.expose().to_owned();
    assert!(token.starts_with("hwi_") && token.len() == 4 + 43, "{token}");
    assert_eq!(issued.expires_at, NOW + 7 * 24 * 3600);
    assert!(!format!("{issued:?}").contains(&token), "Debug must not print the token");

    let invites = f.store.read(|tx| tx.invite_list()).expect("list");
    assert_eq!(invites.len(), 1);
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(&token[4..]).expect("decode");
    assert_eq!(invites[0].token_hash, hash_token(&body));
    assert_eq!(invites[0].expires_at, NOW + 604_800);
    assert_eq!(invites[0].account_id, None);
    assert_eq!(invites[0].role, Role::Member);
    assert_eq!(invites[0].created_by, admin.id);
    assert_eq!(invites[0].redeemed_at, None);
    // The database holds the hash, never the token.
    for file in walk(f.scratch.path()) {
        let bytes = std::fs::read(&file).expect("read");
        assert!(!bytes.windows(token.len()).any(|w| w == token.as_bytes()), "token found in {}", file.display());
    }
}

#[test]
fn invite_create_is_refused_for_anyone_but_an_active_administrator() {
    let f = fixture("accounts-invite-refused");
    let root = first_admin(&f);
    let bob = member(&f, &root, "bob");
    // A member, an unknown id.
    for actor in [bob.id, AccountId::from_raw(999)] {
        let e = f.accounts.invite_create(actor, None, None, Role::Member).expect_err("refused");
        assert_eq!(e, AccountError::NotAllowed);
    }
    // A second administrator who is then disabled.
    f.accounts.set_role(root.id, None, bob.id, Role::Admin).expect("promote");
    f.accounts.disable(root.id, None, bob.id).expect("disable");
    assert_eq!(f.accounts.invite_create(bob.id, None, None, Role::Member).expect_err("disabled"), AccountError::NotAllowed);
    // A target that does not exist or is disabled.
    assert_eq!(
        f.accounts.invite_create(root.id, None, Some(AccountId::from_raw(999)), Role::Member).expect_err("missing"),
        AccountError::NotFound
    );
    assert_eq!(
        f.accounts.invite_create(root.id, None, Some(bob.id), Role::Member).expect_err("disabled target"),
        AccountError::NotFound
    );
    assert_eq!(f.store.read(|tx| tx.invite_list()).expect("list").iter().filter(|i| i.redeemed_at.is_none()).count(), 0);
}

#[test]
fn redeem_invite_makes_an_active_member_with_the_next_uid_and_spends_the_token() {
    let f = fixture("accounts-redeem");
    let root = first_admin(&f);
    let issued = f.accounts.invite_create(root.id, None, None, Role::Member).expect("invite");
    let token = issued.token.expose().to_owned();
    let bob = f.accounts.redeem_invite(&token, &person("bob", "Bob"), Some(ADDR), ok).expect("redeem");
    assert_eq!(bob.role, Role::Member);
    assert_eq!(bob.state, AccountState::Active);
    assert_eq!(bob.uid, root.uid + 1);
    assert_eq!(bob.username, "bob");
    assert_eq!(bob.display_name, "Bob");
    let again = f.accounts.redeem_invite(&token, &person("eve", "Eve"), None, ok);
    assert_eq!(again.expect_err("spent"), AccountError::NotRedeemable);
    assert_eq!(f.store.read(|tx| tx.account_count()).expect("count"), 2);
}

#[test]
fn a_taken_username_is_a_conflict_and_the_invitation_still_works() {
    let f = fixture("accounts-conflict");
    let root = first_admin(&f);
    member(&f, &root, "bob");
    let issued = f.accounts.invite_create(root.id, None, None, Role::Member).expect("invite");
    let token = issued.token.expose().to_owned();
    let taken = f.accounts.redeem_invite(&token, &person("bob", "Other Bob"), None, ok);
    assert_eq!(taken.expect_err("taken"), AccountError::Conflict);
    // The name of the first administrator is taken too, and a bad name is refused before anything is kept.
    assert_eq!(f.accounts.redeem_invite(&token, &person("root", "Root"), None, ok).expect_err("taken"), AccountError::Conflict);
    assert!(matches!(f.accounts.redeem_invite(&token, &person("Not Valid", "X"), None, ok), Err(AccountError::Invalid(_))));
    let other = f.accounts.redeem_invite(&token, &person("robert", "Robert"), None, ok).expect("another name");
    assert_eq!(other.username, "robert");
}

#[test]
fn an_expired_invitation_fails_like_an_unknown_one() {
    let f = fixture("accounts-expiry");
    let root = first_admin(&f);
    let make = || f.accounts.invite_create(root.id, None, None, Role::Member).expect("invite");
    let (just_in_time, at_expiry, later) = (make(), make(), make());
    f.clock.set(NOW + 604_800 - 1);
    f.accounts.redeem_invite(just_in_time.token.expose(), &person("early", "Early"), None, ok).expect("one second before");
    f.clock.set(NOW + 604_800);
    let expired = f.accounts.redeem_invite(at_expiry.token.expose(), &person("late", "Late"), None, ok).expect_err("expired");
    f.clock.set(NOW + 604_801);
    let later_error = f.accounts.redeem_invite(later.token.expose(), &person("later", "Later"), None, ok).expect_err("expired");
    let unknown = f
        .accounts
        .redeem_invite(&format!("hwi_{}", "A".repeat(43)), &person("nobody", "Nobody"), None, ok)
        .expect_err("unknown");
    for malformed in ["", "hwi_", "hwi_short", &format!("hwi_{}", "!".repeat(43)), &format!("xyz_{}", "A".repeat(43))] {
        assert_eq!(f.accounts.redeem_invite(malformed, &person("m", "M"), None, ok).expect_err("malformed"), AccountError::NotRedeemable);
    }
    // A bootstrap-shaped token does not redeem an invitation.
    assert_eq!(
        f.accounts.redeem_invite(&format!("hwb_{}", "A".repeat(43)), &person("m", "M"), None, ok).expect_err("wrong kind"),
        AccountError::NotRedeemable
    );
    assert_eq!((expired.clone(), later_error), (AccountError::NotRedeemable, AccountError::NotRedeemable));
    assert_eq!(unknown, expired);
    assert_eq!(unknown.to_string(), expired.to_string());
}

#[test]
fn a_failing_enrolment_rolls_back_and_the_invitation_stays_valid() {
    let f = fixture("accounts-rollback-invite");
    let root = first_admin(&f);
    let issued = f.accounts.invite_create(root.id, None, None, Role::Member).expect("invite");
    let token = issued.token.expose().to_owned();
    let failed = f.accounts.redeem_invite(&token, &person("bob", "Bob"), None, |tx: &Tx<'_>, a: &Account| {
        // The enrolment wrote something before it failed; that goes too.
        tx.totp_set(a.id, b"sealed")?;
        Err::<(), AccountError>(AccountError::Enrolment("the code did not match"))
    });
    assert_eq!(failed.expect_err("failed"), AccountError::Enrolment("the code did not match"));
    assert_eq!(f.store.read(|tx| tx.account_count()).expect("count"), 1);
    assert_eq!(f.store.read(|tx| tx.account_by_username("bob")).expect("read"), None);
    f.accounts.redeem_invite(&token, &person("bob", "Bob"), None, ok).expect("the token still works");
}

#[test]
fn an_invitation_for_an_existing_account_enrols_into_it() {
    let f = fixture("accounts-existing");
    let root = first_admin(&f);
    let bob = member(&f, &root, "bob");
    let issued = f.accounts.invite_create(root.id, None, Some(bob.id), Role::Admin).expect("invite");
    let mut seen = None;
    let got = f
        .accounts
        .redeem_invite(issued.token.expose(), &person("someone-else", "Someone Else"), None, |_, a| -> Result<(), StoreError> {
            seen = Some((a.id, a.username.clone(), a.role));
            Ok(())
        })
        .expect("redeem");
    assert_eq!(seen, Some((bob.id, "bob".to_owned(), Role::Member)));
    assert_eq!(got, bob, "the account is the one it was, role and name included");
    assert_eq!(account_of(&f, bob.id), bob);
    assert_eq!(f.store.read(|tx| tx.account_count()).expect("count"), 2);
}

#[test]
fn an_invitation_of_a_demoted_or_disabled_administrator_stops_working() {
    let f = fixture("accounts-inviter");
    let root = first_admin(&f);
    let second = member(&f, &root, "second");
    f.accounts.set_role(root.id, None, second.id, Role::Admin).expect("promote");
    let mut kept = Vec::new();
    for _ in 0..2 {
        kept.push(f.accounts.invite_create(second.id, None, None, Role::Admin).expect("invite"));
    }
    f.accounts.set_role(root.id, None, second.id, Role::Member).expect("demote");
    let denied = f.accounts.redeem_invite(kept[0].token.expose(), &person("x", "X"), None, ok);
    assert_eq!(denied.expect_err("demoted"), AccountError::NotRedeemable);
    f.accounts.set_role(root.id, None, second.id, Role::Admin).expect("promote again");
    f.accounts.disable(root.id, None, second.id).expect("disable");
    let denied = f.accounts.redeem_invite(kept[1].token.expose(), &person("x", "X"), None, ok);
    assert_eq!(denied.expect_err("disabled"), AccountError::NotRedeemable);
    assert_eq!(f.store.read(|tx| tx.account_count()).expect("count"), 2, "no account came out of them");
}

// ---------------------------------------------------------------------------------------------------------------------
// What an administrator does to an account
// ---------------------------------------------------------------------------------------------------------------------

#[test]
fn create_account_makes_an_account_without_credentials_and_one_invitation() {
    let f = fixture("accounts-create");
    let root = first_admin(&f);
    let (carol, issued) = f.accounts.create_account(root.id, Some(ADDR), "carol", "Carol", Role::Member).expect("create");
    assert_eq!((carol.username.as_str(), carol.role, carol.state), ("carol", Role::Member, AccountState::Active));
    f.store
        .read(|tx| {
            assert_eq!(tx.password_get(carol.id)?, None);
            assert_eq!(tx.totp_get(carol.id)?, None);
            assert_eq!(tx.passkey_count(carol.id)?, 0);
            assert_eq!(tx.unlock_list(carol.id)?.len(), 0);
            let invites = tx.invite_list()?;
            assert_eq!(invites.len(), 1);
            assert_eq!(invites[0].account_id, Some(carol.id));
            assert_eq!(invites[0].created_by, root.id);
            Ok(())
        })
        .expect("read");
    let lines = audit_lines(f.scratch.path());
    assert_eq!(lines.iter().filter(|l| l["action"] == "account.create").count(), 1);
    let line = lines.iter().find(|l| l["action"] == "account.create").expect("line");
    assert_eq!((line["result"].as_str(), line["target"].as_str()), (Some("ok"), Some(carol.id.to_string().as_str())));
    // The invitation enrols into the new account.
    let mut seen = None;
    f.accounts
        .redeem_invite(issued.token.expose(), &person("ignored", "Ignored"), None, |_, a| -> Result<(), StoreError> {
            seen = Some(a.id);
            Ok(())
        })
        .expect("redeem");
    assert_eq!(seen, Some(carol.id));
    // A taken name is a conflict and creates nothing; a member cannot create.
    assert_eq!(f.accounts.create_account(root.id, None, "carol", "Again", Role::Member).expect_err("taken"), AccountError::Conflict);
    assert_eq!(f.accounts.create_account(carol.id, None, "dave", "Dave", Role::Member).expect_err("member"), AccountError::NotAllowed);
    assert_eq!(f.store.read(|tx| tx.account_by_username("dave")).expect("read"), None);
}

/// A recording observer.
#[derive(Default)]
struct Recorder(std::sync::Mutex<Vec<(AccountId, u32, Ending)>>);

impl AccountObserver for Recorder {
    fn account_ended(&self, account: AccountId, uid: u32, ending: Ending) {
        self.0.lock().expect("lock").push((account, uid, ending));
    }
}

fn session_for(f: &Fixture, account: AccountId, seed: u8) {
    f.store
        .write(|tx| {
            tx.session_create(&NewSession {
                id_hash: hash_token(&[seed; 8]),
                account_id: account,
                method: SignInMethod::PasswordTotp,
                now: NOW,
                absolute_expires: NOW + 86_400,
                device: "test",
                addr: ADDR,
            })
            .map(|_| ())
        })
        .expect("session");
}

fn sessions_of(f: &Fixture, account: AccountId) -> usize {
    f.store.read(|tx| tx.session_list(account)).expect("sessions").len()
}

#[test]
fn every_administrator_action_writes_exactly_one_audit_line() {
    let observer = Arc::new(Recorder::default());
    let f = fixture_observed("accounts-audit-table", Some(observer.clone()));
    let root = first_admin(&f);
    let (carol, issued) = f.accounts.create_account(root.id, Some(ADDR), "carol", "Carol", Role::Member).expect("create");
    f.accounts.redeem_invite(issued.token.expose(), &person("carol", "Carol"), Some(ADDR), ok).expect("redeem");
    f.accounts.invite_create(root.id, Some(ADDR), None, Role::Member).expect("invite");
    f.accounts.set_role(root.id, Some(ADDR), carol.id, Role::Admin).expect("role");
    f.accounts.disable(root.id, Some(ADDR), carol.id).expect("disable");
    f.accounts.enable(root.id, Some(ADDR), carol.id).expect("enable");
    f.accounts.delete(root.id, Some(ADDR), carol.id).expect("delete");

    let lines = audit_lines(f.scratch.path());
    println!("Audit table:");
    for l in &lines {
        println!("  {} {} actor={} target={} detail={}", l["action"], l["result"], l["actor"], l["target"], l["detail"]);
    }
    let table: Vec<(&str, &str, String, String)> = lines
        .iter()
        .map(|l| {
            (
                l["action"].as_str().expect("action"),
                l["result"].as_str().expect("result"),
                l["actor"].as_str().expect("actor").to_owned(),
                l["target"].as_str().expect("target").to_owned(),
            )
        })
        .collect();
    let (r, c) = (root.id.to_string(), carol.id.to_string());
    let expected = vec![
        ("bootstrap.redeem", "ok", r.clone(), r.clone()),
        ("account.create", "ok", r.clone(), c.clone()),
        ("account.redeem", "ok", c.clone(), c.clone()),
        ("account.invite", "ok", r.clone(), String::new()),
        ("account.role", "ok", r.clone(), c.clone()),
        ("account.disable", "ok", r.clone(), c.clone()),
        ("account.enable", "ok", r.clone(), c.clone()),
        ("account.delete", "ok", r.clone(), c.clone()),
    ];
    assert_eq!(table, expected);
    assert!(lines.iter().all(|l| l["addr"] == "192.0.2.7" || l["action"] == "bootstrap.redeem"));
}

#[test]
fn a_refused_call_writes_one_fail_line_and_changes_nothing() {
    let f = fixture("accounts-audit-fail");
    let root = first_admin(&f);
    let bob = member(&f, &root, "bob");
    let before = audit_lines(f.scratch.path()).len();

    let refused: Vec<(&str, AccountError)> = vec![
        ("account.role", f.accounts.set_role(root.id, None, root.id, Role::Member).expect_err("last admin")),
        ("account.disable", f.accounts.disable(root.id, None, root.id).expect_err("last admin")),
        ("account.delete", f.accounts.delete(root.id, None, root.id).expect_err("last admin")),
        ("account.enable", f.accounts.enable(bob.id, None, bob.id).expect_err("member")),
        ("account.delete", f.accounts.delete(root.id, None, AccountId::from_raw(999)).expect_err("missing")),
        ("account.invite", f.accounts.invite_create(bob.id, None, None, Role::Member).expect_err("member")),
    ];
    assert_eq!(refused[0].1, AccountError::LastAdmin);
    assert_eq!(refused[0].1.to_string(), "the last active administrator cannot be demoted, disabled or deleted");
    let lines = audit_lines(f.scratch.path());
    assert_eq!(lines.len() - before, refused.len(), "one line per refused call");
    for (line, (action, error)) in lines[before..].iter().zip(&refused) {
        assert_eq!(line["action"], *action);
        assert_eq!(line["result"], "fail", "{error}");
    }
    assert_eq!(lines[before]["detail"]["reason"], "last_admin");
    assert_eq!(account_of(&f, root.id), root, "nothing changed");
    assert_eq!(f.store.read(|tx| tx.admin_count_active()).expect("count"), 1);
}

#[test]
fn disable_and_delete_end_the_sessions_and_tell_the_observer_once_after_the_commit() {
    let observer = Arc::new(Recorder::default());
    let f = fixture_observed("accounts-observer", Some(observer.clone()));
    let root = first_admin(&f);
    let bob = member(&f, &root, "bob");
    let carol = member(&f, &root, "carol");
    for (account, seed) in [(bob.id, 1u8), (bob.id, 2), (carol.id, 3), (carol.id, 4), (root.id, 5)] {
        session_for(&f, account, seed);
    }
    f.accounts.disable(root.id, None, bob.id).expect("disable");
    assert_eq!(sessions_of(&f, bob.id), 0);
    assert_eq!(sessions_of(&f, carol.id), 2, "other accounts keep theirs");
    f.accounts.delete(root.id, None, carol.id).expect("delete");
    assert_eq!(sessions_of(&f, carol.id), 0);
    assert_eq!(sessions_of(&f, root.id), 1);
    let calls = observer.0.lock().expect("lock").clone();
    println!("Observer: {calls:?}");
    assert_eq!(calls, vec![(bob.id, bob.uid, Ending::Disabled), (carol.id, carol.uid, Ending::Deleted)]);
    // A refused call tells nobody; enabling does not either.
    let _ = f.accounts.disable(root.id, None, root.id);
    f.accounts.enable(root.id, None, bob.id).expect("enable");
    assert_eq!(observer.0.lock().expect("lock").len(), 2);
}

#[test]
fn an_observer_that_panics_does_not_undo_the_change() {
    struct Panics;
    impl AccountObserver for Panics {
        fn account_ended(&self, _: AccountId, _: u32, _: Ending) {
            panic!("the observer failed");
        }
    }
    let f = fixture_observed("accounts-observer-panic", Some(Arc::new(Panics)));
    let root = first_admin(&f);
    let bob = member(&f, &root, "bob");
    session_for(&f, bob.id, 1);
    f.accounts.disable(root.id, None, bob.id).expect("the call succeeds");
    assert_eq!(account_of(&f, bob.id).state, AccountState::Disabled);
    assert_eq!(sessions_of(&f, bob.id), 0);
    f.accounts.delete(root.id, None, bob.id).expect("the call succeeds");
    assert_eq!(account_of(&f, bob.id).state, AccountState::Deleted);
    let lines = audit_lines(f.scratch.path());
    assert_eq!(lines.last().expect("line")["action"], "account.delete");
    assert_eq!(lines.last().expect("line")["result"], "ok");
}

#[test]
fn a_deleted_account_keeps_its_row_name_and_uid_and_its_home_is_left_alone() {
    let f = fixture("accounts-delete");
    let root = first_admin(&f);
    let bob = member(&f, &root, "bob");
    let home = f.scratch.join("homes").join(bob.uid.to_string());
    std::fs::create_dir_all(&home).expect("home");
    std::fs::write(home.join("notes.txt"), b"mine").expect("file");
    let store_file = f.store.path().to_owned();
    f.accounts.delete(root.id, None, bob.id).expect("delete");
    let row = account_of(&f, bob.id);
    assert_eq!((row.state, row.uid, row.username.as_str()), (AccountState::Deleted, bob.uid, "bob"));
    assert_eq!(std::fs::read(home.join("notes.txt")).expect("the home is untouched"), b"mine");
    assert!(store_file.exists());
    // The name is reserved and the next account gets a new uid.
    assert_eq!(f.accounts.create_account(root.id, None, "bob", "Bob", Role::Member).expect_err("reserved"), AccountError::Conflict);
    let next = member(&f, &root, "dave");
    assert_eq!(next.uid, bob.uid + 1);
    // A deleted account can be neither enabled, disabled, deleted nor changed again.
    for result in [
        f.accounts.enable(root.id, None, bob.id),
        f.accounts.disable(root.id, None, bob.id),
        f.accounts.delete(root.id, None, bob.id),
        f.accounts.set_role(root.id, None, bob.id, Role::Admin),
    ] {
        assert_eq!(result.expect_err("deleted"), AccountError::NotFound);
    }
}

#[test]
fn no_audit_line_holds_a_token_or_a_name() {
    let f = fixture("accounts-audit-secrets");
    let root = first_admin(&f);
    let issued = f.accounts.bootstrap_issue();
    assert_eq!(issued.expect_err("an account exists"), AccountError::AlreadySetUp);
    let (carol, made) = f.accounts.create_account(root.id, Some(ADDR), "carol-secret-name", "Carol Display", Role::Member).expect("create");
    let invited = f.accounts.invite_create(root.id, Some(ADDR), Some(carol.id), Role::Member).expect("invite");
    let wrong = f.accounts.redeem_invite("hwi_wrong-token-text-wrong-token-text-wrong-tok", &person("zed-name", "Zed"), Some(ADDR), ok);
    assert!(wrong.is_err());
    f.accounts.redeem_invite(made.token.expose(), &person("x", "x"), Some(ADDR), ok).expect("redeem");
    f.accounts.disable(root.id, Some(ADDR), carol.id).expect("disable");
    let all: String = std::fs::read_dir(f.scratch.join("server/audit"))
        .expect("dir")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("read"))
        .collect();
    for secret in [made.token.expose(), invited.token.expose(), "hwi_", "hwb_", "carol-secret-name", "Carol Display", "zed-name", "root"] {
        assert!(!all.contains(secret), "the audit file holds {secret:?}");
    }
    assert!(all.lines().count() >= 6);
}
