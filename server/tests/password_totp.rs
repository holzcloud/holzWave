//! Passwords and one-time codes: the split derivation, the codes and the window they are accepted in.

#[path = "support/scratch.rs"]
mod scratch;

use holzwave_server::auth::password::{PasswordError, enrol_password, verify_password};
use holzwave_server::auth::totp::{self, TotpError};
use holzwave_server::keyfile::ServerKey;
use holzwave_server::sealed::Sealer;
use holzwave_server::store::{AccountId, NewAccount, Role, Store};
use holzwave_server::unlock::Kdf;
use scratch::Scratch;

const NOW: i64 = 1_772_353_800;
const RANGE: std::ops::RangeInclusive<u32> = 20000..=29999;

fn sealer() -> Sealer {
    Sealer::new(&ServerKey::from_bytes([7u8; 32]).expect("a key that is not all zero"))
}

fn account(store: &Store, name: &str) -> AccountId {
    store.write(|tx| tx.account_create(&NewAccount::new(name, Role::Member), RANGE, NOW)).expect("account").id
}

#[test]
fn tracer_a_password_and_a_code_sign_one_account_in() {
    let scratch = Scratch::new("pw-tracer");
    let store = Store::open(scratch.path()).expect("open");
    let sealer = sealer();
    let alice = account(&store, "alice");
    let kdf = Kdf::lower_bound();

    let enrolled = store.write(|tx| Ok(enrol_password(tx, alice, "correct horse battery", &kdf, NOW))).expect("write");
    let enrolled = enrolled.expect("enrol");
    let row = store.read(|tx| tx.password_get(alice)).expect("read").expect("a row");
    println!("Row: {}", row.params);
    assert_eq!(row.verifier.len(), 32);
    assert_eq!(row.salt.len(), 16);

    let (id, again) = store.read(|tx| Ok(verify_password(tx, "alice", "correct horse battery"))).expect("read").expect("verify");
    assert_eq!(id, alice);
    let keys_equal = enrolled.same_as(&again);
    println!("Tracer: key equal = {keys_equal}");
    assert!(keys_equal);
    let wrong = store.read(|tx| Ok(verify_password(tx, "alice", "wrong horse battery"))).expect("read");
    assert_eq!(wrong.expect_err("wrong password"), PasswordError::InvalidCredentials);

    let secret = totp::generate_secret().expect("secret");
    assert_eq!(secret.len(), 20);
    let sealed = sealer.seal(&totp::seal_context(alice), &secret[..]).expect("seal");
    store.write(|tx| tx.totp_set(alice, &sealed)).expect("store the secret");
    let code = totp::code_at(&secret[..], NOW).expect("code");
    store.write(|tx| Ok(totp::verify_and_consume(tx, &sealer, alice, &code, NOW))).expect("write").expect("first use");
    let second = store.write(|tx| Ok(totp::verify_and_consume(tx, &sealer, alice, &code, NOW))).expect("write");
    println!("Tracer: second use = {second:?}");
    assert_eq!(second.expect_err("replay"), TotpError::InvalidCode);
}
