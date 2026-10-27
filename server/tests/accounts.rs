//! Accounts: the first administrator, invitations, the administrator actions and single sign-on accounts.

#[path = "support/scratch.rs"]
mod scratch;

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Barrier};
use std::thread;

use holzwave_server::accounts::{
    AccountError, AccountObserver, Accounts, Ending, NewAccount, OidcClaims, ProviderRules, derive_username,
};
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

// ---------------------------------------------------------------------------------------------------------------------
// Single sign-on
// ---------------------------------------------------------------------------------------------------------------------

const ISSUER: &str = "https://idp.example.test";

fn claims(issuer: &str, subject: &str, email: Option<&str>, verified: bool, groups: &[&str]) -> OidcClaims {
    OidcClaims {
        issuer: issuer.to_owned(),
        subject: subject.to_owned(),
        email: email.map(str::to_owned),
        email_verified: verified,
        preferred_username: None,
        display_name: None,
        groups: groups.iter().map(|g| (*g).to_owned()).collect(),
    }
}

fn rules(domains: &[&str], groups: &[&str]) -> ProviderRules {
    ProviderRules {
        allowed_domains: domains.iter().map(|d| (*d).to_owned()).collect(),
        allowed_groups: groups.iter().map(|g| (*g).to_owned()).collect(),
    }
}

fn identities(f: &Fixture) -> usize {
    f.store
        .read(|tx| Ok(tx.account_list()?.iter().map(|a| tx.oidc_list(a.id).map(|l| l.len())).collect::<Result<Vec<_>, _>>()?.iter().sum()))
        .expect("count")
}

fn account_count(f: &Fixture) -> i64 {
    f.store.read(|tx| tx.account_count()).expect("count")
}

#[test]
fn a_listed_domain_makes_a_member_and_the_identity_is_matched_by_issuer_and_subject() {
    let f = fixture("accounts-oidc-domain");
    let root = first_admin(&f);
    let by_domain = rules(&["example.org"], &[]);

    let first = f
        .accounts
        .oidc_sign_in(&claims(ISSUER, "subject-1", Some("a@Example.ORG"), true, &[]), &by_domain, Some(ADDR))
        .expect("created");
    assert_eq!((first.role, first.state, first.username.as_str()), (Role::Member, AccountState::Active, "a"));
    assert_eq!(first.uid, root.uid + 1);
    assert_eq!(identities(&f), 1);
    let lines = audit_lines(f.scratch.path());
    let created: Vec<_> = lines.iter().filter(|l| l["action"] == "oidc.create").collect();
    assert_eq!(created.len(), 1);
    assert_eq!((created[0]["result"].as_str(), created[0]["target"].as_str()), (Some("ok"), Some(first.id.to_string().as_str())));

    let again = f
        .accounts
        .oidc_sign_in(&claims(ISSUER, "subject-1", Some("a@Example.ORG"), true, &[]), &by_domain, Some(ADDR))
        .expect("found");
    assert_eq!(again, first);
    // The same subject with another e-mail, an unverified one, or after the lists were emptied is the same account.
    let moved = f
        .accounts
        .oidc_sign_in(&claims(ISSUER, "subject-1", Some("a@other.org"), false, &[]), &rules(&[], &[]), None)
        .expect("found by issuer and subject");
    assert_eq!(moved, first);
    assert_eq!(account_count(&f), 2);
    assert_eq!(identities(&f), 1);
    assert_eq!(audit_lines(f.scratch.path()).iter().filter(|l| l["action"] == "oidc.create").count(), 1);
}

#[test]
fn an_account_is_never_taken_over_by_an_e_mail_or_a_name() {
    let f = fixture("accounts-oidc-takeover");
    let root = first_admin(&f);
    // A local account called `a`, and another whose display name is the e-mail address.
    let a = member(&f, &root, "a");
    let by_domain = rules(&["example.org"], &[]);
    let mut stranger = claims(ISSUER, "subject-2", Some("a@example.org"), true, &[]);
    stranger.preferred_username = Some("root".to_owned());
    stranger.display_name = Some("Root".to_owned());
    let got = f.accounts.oidc_sign_in(&stranger, &by_domain, None).expect("a new account");
    assert_ne!(got.id, a.id);
    assert_ne!(got.id, root.id);
    assert_eq!(got.role, Role::Member);
    assert_eq!(got.username, "root-2", "the name is derived and made unique, never shared");
    assert_eq!(account_of(&f, a.id), a);
    assert_eq!(account_of(&f, root.id), root);
    // The stranger is the only identity.
    assert_eq!(f.store.read(|tx| tx.oidc_list(got.id)).expect("list").len(), 1);
    assert_eq!(f.store.read(|tx| tx.oidc_list(a.id)).expect("list").len(), 0);
}

#[test]
fn an_unverified_e_mail_or_an_unlisted_subject_makes_nothing() {
    let f = fixture("accounts-oidc-denied");
    first_admin(&f);
    let by_both = rules(&["example.org"], &["staff"]);
    let denied: Vec<(&str, OidcClaims)> = vec![
        ("unverified", claims(ISSUER, "s1", Some("a@example.org"), false, &[])),
        ("other domain", claims(ISSUER, "s2", Some("a@other.org"), true, &[])),
        ("sub-domain", claims(ISSUER, "s3", Some("a@sub.example.org"), true, &[])),
        ("suffix", claims(ISSUER, "s4", Some("a@evilexample.org"), true, &[])),
        ("trailing", claims(ISSUER, "s5", Some("a@example.org.evil.test"), true, &[])),
        ("no at sign", claims(ISSUER, "s6", Some("example.org"), true, &[])),
        ("no local part", claims(ISSUER, "s7", Some("@example.org"), true, &[])),
        ("no e-mail", claims(ISSUER, "s8", None, true, &["x"])),
        ("group in another case", claims(ISSUER, "s9", None, false, &["Staff"])),
        ("group as part", claims(ISSUER, "s10", None, false, &["staff-extra", "sta", "staff "])),
        ("empty subject", claims(ISSUER, "", Some("a@example.org"), true, &[])),
        ("empty issuer", claims("", "s11", Some("a@example.org"), true, &[])),
        ("long subject", claims(ISSUER, &"s".repeat(256), Some("a@example.org"), true, &[])),
        ("control in subject", claims(ISSUER, "s\n12", Some("a@example.org"), true, &[])),
    ];
    println!("OIDC table:");
    for (name, c) in &denied {
        let r = f.accounts.oidc_sign_in(c, &by_both, Some(ADDR));
        println!("  {name}: denied = {}", r.as_ref().err() == Some(&AccountError::Denied));
        assert_eq!(r.expect_err(name), AccountError::Denied, "{name}");
    }
    assert_eq!(account_count(&f), 1);
    assert_eq!(identities(&f), 0);
    let lines = audit_lines(f.scratch.path());
    let fails = lines.iter().filter(|l| l["action"] == "oidc.create" && l["result"] == "fail").count();
    assert_eq!(fails, denied.len(), "one failed line per refusal");
    // No claim value is in the file.
    let all = std::fs::read_dir(f.scratch.join("server/audit"))
        .expect("dir")
        .map(|e| std::fs::read_to_string(e.expect("entry").path()).expect("read"))
        .collect::<String>();
    assert!(!all.contains("example.org") && !all.contains("staff") && !all.contains(ISSUER));
}

#[test]
fn a_listed_group_is_enough_and_groups_are_compared_exactly() {
    let f = fixture("accounts-oidc-groups");
    first_admin(&f);
    let by_group = rules(&[], &["staff"]);
    let mut c = claims(ISSUER, "g1", None, false, &["staff", "x"]);
    c.preferred_username = Some("Gina".to_owned());
    let gina = f.accounts.oidc_sign_in(&c, &by_group, None).expect("created");
    assert_eq!((gina.username.as_str(), gina.role), ("gina", Role::Member));
    let different_case = claims(ISSUER, "g2", None, false, &["Staff"]);
    assert_eq!(f.accounts.oidc_sign_in(&different_case, &by_group, None).expect_err("case"), AccountError::Denied);
    // A domain list does not let a group in, and a group list does not let a verified e-mail in.
    let by_domain = rules(&["example.org"], &[]);
    assert_eq!(f.accounts.oidc_sign_in(&claims(ISSUER, "g3", None, false, &["staff"]), &by_domain, None).expect_err("domain only"), AccountError::Denied);
    assert_eq!(
        f.accounts.oidc_sign_in(&claims(ISSUER, "g4", Some("a@example.org"), true, &[]), &by_group, None).expect_err("group only"),
        AccountError::Denied
    );
    assert_eq!(account_count(&f), 2);
}

#[test]
fn with_both_lists_empty_no_account_is_made_but_a_linked_identity_still_signs_in() {
    let f = fixture("accounts-oidc-empty");
    first_admin(&f);
    let open = claims(ISSUER, "e1", Some("a@example.org"), true, &["staff"]);
    assert_eq!(f.accounts.oidc_sign_in(&open, &rules(&[], &[]), None).expect_err("empty lists"), AccountError::Denied);
    assert_eq!(account_count(&f), 1);
    let made = f.accounts.oidc_sign_in(&open, &rules(&["example.org"], &[]), None).expect("created");
    let back = f.accounts.oidc_sign_in(&open, &rules(&[], &[]), None).expect("linked identity");
    assert_eq!(back, made);
}

#[test]
fn the_same_subject_from_another_issuer_is_another_identity() {
    let f = fixture("accounts-oidc-issuers");
    first_admin(&f);
    let by_domain = rules(&["example.org"], &[]);
    let one = f.accounts.oidc_sign_in(&claims(ISSUER, "same", Some("a@example.org"), true, &[]), &by_domain, None).expect("one");
    let two = f
        .accounts
        .oidc_sign_in(&claims("https://other-idp.example.test", "same", Some("a@example.org"), true, &[]), &by_domain, None)
        .expect("two");
    assert_ne!(one.id, two.id);
    assert_eq!((one.username.as_str(), two.username.as_str()), ("a", "a-2"));
    assert_eq!(account_of(&f, one.id), one, "the first is not touched");
    assert_eq!(identities(&f), 2);
    // Another issuer that is not allowed by the rules of this call still needs the rules.
    assert_eq!(
        f.accounts.oidc_sign_in(&claims("https://third.example.test", "same", Some("a@nowhere.test"), true, &[]), &by_domain, None).expect_err("denied"),
        AccountError::Denied
    );
}

#[test]
fn the_identity_of_a_disabled_account_is_refused() {
    let f = fixture("accounts-oidc-disabled");
    let root = first_admin(&f);
    let by_domain = rules(&["example.org"], &[]);
    let c = claims(ISSUER, "d1", Some("a@example.org"), true, &[]);
    let a = f.accounts.oidc_sign_in(&c, &by_domain, None).expect("created");
    f.accounts.disable(root.id, None, a.id).expect("disable");
    assert_eq!(f.accounts.oidc_sign_in(&c, &by_domain, None).expect_err("disabled"), AccountError::Denied);
    // It makes no second account for the same subject either.
    assert_eq!(account_count(&f), 2);
    f.accounts.enable(root.id, None, a.id).expect("enable");
    assert_eq!(f.accounts.oidc_sign_in(&c, &by_domain, None).expect("enabled again"), account_of(&f, a.id));
    // A deleted account's identity is gone with it, and its name stays reserved.
    f.accounts.delete(root.id, None, a.id).expect("delete");
    let reborn = f.accounts.oidc_sign_in(&c, &by_domain, None).expect("a new account, because the link went with the old one");
    assert_ne!(reborn.id, a.id);
    assert_eq!(reborn.username, "a-2");
}

#[test]
fn claims_print_nothing_about_a_person() {
    let mut c = claims(ISSUER, "private-subject", Some("private@example.org"), true, &["private-group"]);
    c.preferred_username = Some("private-name".to_owned());
    let shown = format!("{c:?}");
    for word in ["private", "example", "idp"] {
        assert!(!shown.contains(word), "{shown}");
    }
}

#[test]
fn username_derivation_table() {
    let long = "a".repeat(60);
    let dotted_cut = format!("{}.bbb", "a".repeat(31));
    let cases: Vec<(Option<&str>, Option<&str>, String)> = vec![
        (Some("Alice.Smith"), None, "alice.smith".to_owned()),
        (Some("a b@c"), None, "a-b-c".to_owned()),
        (Some(""), Some("x@y.z"), "x".to_owned()),
        (None, Some("X@y.z"), "x".to_owned()),
        (Some(long.as_str()), None, "a".repeat(32)),
        (Some(".hidden"), None, "hidden".to_owned()),
        (Some("end."), None, "end".to_owned()),
        (Some("..."), None, "user".to_owned()),
        (Some(dotted_cut.as_str()), None, "a".repeat(31)),
        (None, None, "user".to_owned()),
        (Some(""), Some(""), "user".to_owned()),
        (Some("Ünï Cödé"), None, "-n--c-d-".to_owned()),
        (Some("\u{c4}\u{d6} \u{dc}"), Some("kim@y.z"), "kim".to_owned()),
        (Some("../../etc/passwd"), None, "-..-etc-passwd".to_owned()),
        (Some("admin\n"), None, "admin-".to_owned()),
        (None, Some("first.last+tag@example.org"), "first.last-tag".to_owned()),
        (None, Some("a@b@example.org"), "a-b".to_owned()),
        (Some("UPPER_case-1"), None, "upper_case-1".to_owned()),
    ];
    println!("Usernames:");
    for (preferred, email, expected) in cases {
        let got = derive_username(preferred, email);
        println!("  {preferred:?} + {email:?} -> {got:?}");
        assert_eq!(got, expected, "{preferred:?} {email:?}");
        assert!(holzwave_server::store::valid_username(&got), "{got:?} must be a username");
        assert!(got.chars().count() <= 32);
    }
}

#[test]
fn a_collision_gets_a_number_up_to_ninety_nine_and_then_a_refusal() {
    let f = fixture("accounts-oidc-collision");
    let root = first_admin(&f);
    let by_domain = rules(&["example.org"], &[]);
    // `root` is taken: the first just-in-time `root` becomes `root-2`.
    let mut c = claims(ISSUER, "c1", None, true, &[]);
    c.email = Some("root@example.org".to_owned());
    let two = f.accounts.oidc_sign_in(&c, &by_domain, None).expect("root-2");
    assert_eq!(two.username, "root-2");
    // Fill `root-3` .. `root-99` through an administrator, then the 99th collision is a refusal.
    for n in 3..=99 {
        f.accounts.create_account(root.id, None, &format!("root-{n}"), "x", Role::Member).expect("fill");
    }
    let before = (account_count(&f), identities(&f));
    let mut next = claims(ISSUER, "c100", None, true, &[]);
    next.email = Some("root@example.org".to_owned());
    assert_eq!(f.accounts.oidc_sign_in(&next, &by_domain, None).expect_err("exhausted"), AccountError::Denied);
    assert_eq!((account_count(&f), identities(&f)), before, "nothing was made");
}

// ---------------------------------------------------------------------------------------------------------------------
// Races and rules
// ---------------------------------------------------------------------------------------------------------------------

const THREADS: usize = 16;

/// Runs `job(i)` on 16 threads released together and returns the results in thread order.
fn together<T: Send + 'static>(job: impl Fn(usize) -> T + Send + Sync + 'static) -> Vec<T> {
    let job = Arc::new(job);
    let barrier = Arc::new(Barrier::new(THREADS));
    let handles: Vec<_> = (0..THREADS)
        .map(|i| {
            let (job, barrier) = (job.clone(), barrier.clone());
            thread::spawn(move || {
                barrier.wait();
                job(i)
            })
        })
        .collect();
    handles.into_iter().map(|h| h.join().expect("thread")).collect()
}

#[test]
fn race_bootstrap() {
    let f = fixture("accounts-race-bootstrap");
    let token = f.accounts.bootstrap_issue().expect("issue").token.expose().to_owned();
    let accounts = Arc::new(f.accounts);
    let results = {
        let accounts = accounts.clone();
        together(move |i| {
            let name = format!("user{i}");
            accounts.redeem_bootstrap(&token, &person(&name, &name), None, ok)
        })
    };
    let winners = results.iter().filter(|r| r.is_ok()).count();
    let losers = results.iter().filter(|r| matches!(r, Err(AccountError::NotRedeemable))).count();
    println!("Races: bootstrap threads {THREADS}, winners {winners}, common error {losers}");
    assert_eq!((winners, losers), (1, THREADS - 1));
    assert_eq!(f.store.read(|tx| tx.account_count()).expect("count"), 1);
    let lines = audit_lines(f.scratch.path());
    assert_eq!(lines.len(), THREADS);
    assert_eq!(lines.iter().filter(|l| l["result"] == "ok").count(), 1);
}

#[test]
fn race_invite() {
    let f = fixture("accounts-race-invite");
    let root = first_admin(&f);
    let token = f.accounts.invite_create(root.id, None, None, Role::Member).expect("invite").token.expose().to_owned();
    let accounts = Arc::new(f.accounts);
    let results = {
        let accounts = accounts.clone();
        together(move |i| {
            let name = format!("user{i}");
            accounts.redeem_invite(&token, &person(&name, &name), None, ok)
        })
    };
    let winners = results.iter().filter(|r| r.is_ok()).count();
    let losers = results.iter().filter(|r| matches!(r, Err(AccountError::NotRedeemable))).count();
    println!("Races: invite threads {THREADS}, winners {winners}, common error {losers}");
    assert_eq!((winners, losers), (1, THREADS - 1));
    assert_eq!(f.store.read(|tx| tx.account_count()).expect("count"), 2);
}

#[test]
fn race_oidc_one_subject() {
    let f = fixture("accounts-race-oidc");
    first_admin(&f);
    let accounts = Arc::new(f.accounts);
    let results = {
        let accounts = accounts.clone();
        together(move |_| {
            accounts.oidc_sign_in(&claims(ISSUER, "same", Some("a@example.org"), true, &[]), &rules(&["example.org"], &[]), None)
        })
    };
    let ids: std::collections::HashSet<_> = results.iter().map(|r| r.as_ref().expect("signed in").id).collect();
    println!("Races: oidc threads {THREADS}, accounts made {}", ids.len());
    assert_eq!(ids.len(), 1);
    assert_eq!(f.store.read(|tx| tx.account_count()).expect("count"), 2);
    assert_eq!(audit_lines(f.scratch.path()).iter().filter(|l| l["action"] == "oidc.create").count(), 1);
}

#[test]
fn same_username_twice() {
    let f = fixture("accounts-same-username");
    let root = first_admin(&f);
    let one = f.accounts.invite_create(root.id, None, None, Role::Member).expect("invite one");
    let two = f.accounts.invite_create(root.id, None, None, Role::Member).expect("invite two");
    let accounts = Arc::new(f.accounts);
    let tokens = Arc::new([one.token.expose().to_owned(), two.token.expose().to_owned()]);
    let results = {
        let (accounts, tokens) = (accounts.clone(), tokens.clone());
        together(move |i| {
            if i >= 2 {
                return None;
            }
            Some(accounts.redeem_invite(&tokens[i], &person("sam", "Sam"), None, ok))
        })
    };
    let results: Vec<_> = results.into_iter().flatten().collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|r| matches!(r, Err(AccountError::Conflict))).count(), 1);
    assert_eq!(f.store.read(|tx| tx.account_count()).expect("count"), 2);
    // The loser's invitation still works under another name.
    let loser = if results[0].is_err() { &tokens[0] } else { &tokens[1] };
    let other = accounts.redeem_invite(loser, &person("samantha", "Samantha"), None, ok).expect("another name");
    assert_eq!(other.username, "samantha");
}

#[test]
fn uid_not_reused() {
    let f = fixture("accounts-uid");
    let root = first_admin(&f);
    let bob = member(&f, &root, "bob");
    f.accounts.delete(root.id, None, bob.id).expect("delete");
    let carol = member(&f, &root, "carol");
    println!("Uids: root {}, bob {} (deleted), carol {}", root.uid, bob.uid, carol.uid);
    assert_eq!((root.uid, bob.uid, carol.uid), (20000, 20001, 20002));
}

#[test]
fn rollback_keeps_token() {
    let f = fixture("accounts-rollback-bootstrap");
    let token = f.accounts.bootstrap_issue().expect("issue").token.expose().to_owned();
    let failed = f.accounts.redeem_bootstrap(&token, &person("alice", "Alice"), None, |tx: &Tx<'_>, a: &Account| {
        tx.totp_set(a.id, b"sealed")?;
        Err::<(), AccountError>(AccountError::Enrolment("the password was refused"))
    });
    assert_eq!(failed.expect_err("failed"), AccountError::Enrolment("the password was refused"));
    assert_eq!(f.store.read(|tx| tx.account_count()).expect("count"), 0);
    let lines = audit_lines(f.scratch.path());
    assert_eq!((lines.len(), lines[0]["result"].as_str()), (1, Some("fail")));
    let alice = f.accounts.redeem_bootstrap(&token, &person("alice", "Alice"), None, ok).expect("the token still works");
    assert_eq!((alice.role, alice.uid), (Role::Admin, 20000));
    // A panic inside the enrolment leaves nothing behind either.
    let token = {
        let g = fixture("accounts-panic-bootstrap");
        let token = g.accounts.bootstrap_issue().expect("issue").token.expose().to_owned();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = g.accounts.redeem_bootstrap(&token, &person("alice", "Alice"), None, |_: &Tx<'_>, _: &Account| -> Result<(), StoreError> {
                panic!("the enrolment panicked")
            });
        }));
        assert!(caught.is_err());
        assert_eq!(g.store.read(|tx| tx.account_count()).expect("count"), 0);
        g.accounts.redeem_bootstrap(&token, &person("alice", "Alice"), None, ok).expect("still valid after a panic");
        token
    };
    assert!(token.starts_with("hwb_"));
}

#[test]
fn no_default_admin() {
    let f = fixture("accounts-no-default");
    assert_eq!(f.store.read(|tx| tx.account_count()).expect("count"), 0);
    assert_eq!(f.store.read(|tx| tx.admin_count_active()).expect("count"), 0);
    // A token that was issued before an account appeared cannot make an administrator any more.
    let late = f.accounts.bootstrap_issue().expect("issue").token.expose().to_owned();
    f.store
        .write(|tx| tx.account_create(&holzwave_server::store::NewAccount::new("early", Role::Member), RANGE, NOW))
        .expect("an account");
    assert_eq!(
        f.accounts.redeem_bootstrap(&late, &person("late", "Late"), None, ok).expect_err("no longer allowed"),
        AccountError::NotRedeemable
    );
    assert_eq!(f.accounts.bootstrap_issue().expect_err("an account exists"), AccountError::AlreadySetUp);
    assert_eq!(f.store.read(|tx| tx.admin_count_active()).expect("count"), 0, "nobody became an administrator");
    // The same after the first administrator.
    let g = fixture("accounts-no-default-2");
    first_admin(&g);
    assert_eq!(g.accounts.bootstrap_issue().expect_err("an account exists"), AccountError::AlreadySetUp);
}

#[test]
fn the_bootstrap_token_lives_one_hour() {
    let f = fixture("accounts-bootstrap-ttl");
    let issued = f.accounts.bootstrap_issue().expect("issue");
    assert_eq!(issued.expires_at, NOW + 3_600);
    let token = issued.token.expose().to_owned();
    f.clock.set(NOW + 3_600);
    assert_eq!(f.accounts.redeem_bootstrap(&token, &person("a", "A"), None, ok).expect_err("expired"), AccountError::NotRedeemable);
    f.clock.set(NOW + 3_599);
    f.accounts.redeem_bootstrap(&token, &person("a", "A"), None, ok).expect("one second before the end");
}
