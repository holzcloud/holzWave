//! The wire between the vault and the Kotlin Keystore: error words, byte arrays, the device's answers, and the rule that
//! hides the app switcher card. No Android code runs here; the strings are the ones the plugin sends.

use holzwave_core::vault::{KeystoreCaps, KeystoreError, LostReason, Notice, Protection, VaultState, VaultStatus};
use holzwave_host::keystore_wire::{
    FAILED_TEXT, MAX_KEY_BYTES, MAX_WRAPPED_BYTES, RecentsAnswer, RecentsMode, bytes_from_wire, bytes_to_wire,
    caps_from_wire, keystore_error, millis_from_wire, protection_to_wire, recents_from_wire, recents_hidden,
    recents_to_wire,
};
use serde_json::{Value, json};

const CANARY: &str = "HW-CANARY-7f3a91";

fn failed() -> KeystoreError {
    KeystoreError::Failed(FAILED_TEXT.to_owned())
}

#[test]
fn the_named_words_map_and_every_other_word_is_one_fixed_failure() {
    assert_eq!(keystore_error("cancelled"), KeystoreError::Cancelled);
    assert_eq!(keystore_error("key_gone"), KeystoreError::KeyGone);
    assert_eq!(keystore_error("no_screen_lock"), KeystoreError::NoScreenLock);
    assert_eq!(keystore_error("invalid"), KeystoreError::Invalid);
    assert_eq!(keystore_error("failed"), failed());
    let long = "k".repeat(100_000);
    for word in [
        "",
        "Cancelled",
        "cancelled ",
        "cancelled\n",
        "key gone",
        "nonsense",
        "javax.crypto.AEADBadTagException: Tag mismatch at /data/user/0/ch.holzcloud.holzwave/x",
        "bad\u{0}word",
        "esc\u{1b}[31m",
        long.as_str(),
    ] {
        let error = keystore_error(word);
        assert_eq!(error, failed(), "{word:?}");
        assert!(!error.to_string().contains("AEAD") && !format!("{error:?}").contains("data/user"), "never the received text");
    }
}

#[test]
fn bytes_make_a_json_array_of_numbers_and_come_back() {
    assert_eq!(bytes_to_wire(&[]), json!([]));
    assert_eq!(bytes_to_wire(&[0, 7, 255]), json!([0, 7, 255]));
    let all: Vec<u8> = (0..=255).collect();
    assert_eq!(bytes_from_wire(&bytes_to_wire(&all), 256).expect("round trip"), all);
    assert_eq!(bytes_from_wire(&json!([1, 2, 3]), 3).expect("exactly max"), vec![1, 2, 3]);
    assert_eq!(bytes_from_wire(&json!([]), 0).expect("empty"), Vec::<u8>::new());
}

#[test]
fn a_byte_array_that_is_not_one_is_refused() {
    for bad in [
        json!(null),
        json!("abc"),
        json!(7),
        json!({ "0": 1 }),
        json!(true),
        json!([256]),
        json!([1, 70000]),
        json!([-1]),
        json!([1.5]),
        json!([1.0e0]),
        json!(["1"]),
        json!([null]),
        json!([true]),
        json!([[1]]),
        json!([18446744073709551615u64]),
        json!([1, 2, 3, 4]),
    ] {
        assert_eq!(bytes_from_wire(&bad, 3), Err(failed()), "{bad}");
    }
}

#[test]
fn an_array_over_the_bound_is_refused_before_it_is_read() {
    // An element that would also be refused sits at the end: the length decides first, so nothing is walked or allocated.
    let mut items = vec![json!(1); MAX_KEY_BYTES + 1];
    items.push(json!("not a number"));
    assert_eq!(bytes_from_wire(&Value::Array(items), MAX_KEY_BYTES), Err(failed()));
    let wrapped = Value::Array(vec![json!(0); MAX_WRAPPED_BYTES + 1]);
    assert_eq!(bytes_from_wire(&wrapped, MAX_WRAPPED_BYTES), Err(failed()));
    assert_eq!(bytes_from_wire(&Value::Array(vec![json!(0); MAX_WRAPPED_BYTES]), MAX_WRAPPED_BYTES).map(|b| b.len()), Ok(256));
    assert_eq!((MAX_KEY_BYTES, MAX_WRAPPED_BYTES), (32, 256));
}

#[test]
fn the_devices_answer_needs_both_fields_and_defaults_to_nothing() {
    assert_eq!(
        caps_from_wire(&json!({ "has_screen_lock": true, "strongbox": false })),
        Ok(KeystoreCaps { has_screen_lock: true, strongbox: false })
    );
    assert_eq!(
        caps_from_wire(&json!({ "has_screen_lock": false, "strongbox": true, "extra": 1 })),
        Ok(KeystoreCaps { has_screen_lock: false, strongbox: true })
    );
    for bad in [
        json!(null),
        json!([]),
        json!({}),
        json!({ "has_screen_lock": true }),
        json!({ "strongbox": true }),
        json!({ "has_screen_lock": "true", "strongbox": false }),
        json!({ "has_screen_lock": 1, "strongbox": 0 }),
        json!({ "has_screen_lock": null, "strongbox": false }),
    ] {
        assert_eq!(caps_from_wire(&bad), Err(failed()), "{bad}: never a default of true");
    }
}

#[test]
fn the_clock_answer_is_a_whole_number_of_milliseconds() {
    assert_eq!(millis_from_wire(&json!({ "ms": 0 })), Ok(0));
    assert_eq!(millis_from_wire(&json!({ "ms": 123456789 })), Ok(123_456_789));
    for bad in [json!(null), json!({}), json!({ "ms": -1 }), json!({ "ms": 1.5 }), json!({ "ms": "5" }), json!(5)] {
        assert_eq!(millis_from_wire(&bad), Err(failed()), "{bad}");
    }
}

#[test]
fn no_error_carries_what_it_was_given() {
    // The canary stands for a data key, a platform message and a hostile word: none may appear in any result.
    let bytes: Vec<u8> = CANARY.bytes().collect();
    let mut results: Vec<String> = Vec::new();
    let wire = bytes_to_wire(&bytes);
    results.push(format!("{:?}", bytes_from_wire(&wire, 4)));
    let mut with_text = bytes.iter().map(|b| json!(b)).collect::<Vec<_>>();
    with_text.push(json!(CANARY));
    results.push(format!("{:?}", bytes_from_wire(&Value::Array(with_text), 1000)));
    results.push(format!("{:?}", bytes_from_wire(&json!(CANARY), 4)));
    results.push(format!("{:?}", bytes_from_wire(&json!({ CANARY: CANARY }), 4)));
    for word in [CANARY.to_owned(), format!("failed {CANARY}"), format!("cancelled\n{CANARY}")] {
        let error = keystore_error(&word);
        results.push(format!("{error:?}"));
        results.push(error.to_string());
    }
    for value in [
        json!({ "has_screen_lock": CANARY, "strongbox": CANARY }),
        json!({ "has_screen_lock": true, CANARY: CANARY }),
        json!(CANARY),
    ] {
        results.push(format!("{:?}", caps_from_wire(&value)));
    }
    results.push(format!("{:?}", millis_from_wire(&json!({ "ms": CANARY }))));
    results.push(format!("{:?}", recents_from_wire(&json!({ "applied": true, "mode": CANARY }))));
    for text in &results {
        assert!(!text.contains(CANARY), "the canary leaked into {text}");
        let as_numbers = bytes.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(", ");
        assert!(!text.contains(&as_numbers), "the canary bytes leaked into {text}");
    }
    assert!(results.iter().any(|t| t.contains(FAILED_TEXT)), "the errors are the fixed text");
}

#[test]
fn the_protection_names_are_the_vaults() {
    assert_eq!(protection_to_wire(Protection::Auth), "auth");
    assert_eq!(protection_to_wire(Protection::Plain), "plain");
}

#[test]
fn the_recents_rule_follows_the_protection_alone() {
    let mut seen = 0;
    for state in [VaultState::New, VaultState::Locked, VaultState::Unlocked, VaultState::Lost] {
        for protection in [Protection::Auth, Protection::Plain] {
            for reason in [None, Some(LostReason::KeyGone), Some(LostReason::Corrupt)] {
                for (has_screen_lock, strongbox, can_protect, notice) in [
                    (true, false, false, Notice::None),
                    (false, true, true, Notice::NoLock),
                ] {
                    let status = VaultStatus { state, protection, has_screen_lock, strongbox, can_protect, notice, reason };
                    assert_eq!(recents_hidden(&status), protection == Protection::Auth, "{status:?}");
                    seen += 1;
                }
            }
        }
    }
    assert_eq!(seen, 48);
    // The cases the owner named: a protected vault is hidden locked or unlocked, a plain one never.
    let status = |state, protection| VaultStatus {
        state,
        protection,
        has_screen_lock: true,
        strongbox: false,
        can_protect: false,
        notice: Notice::None,
        reason: None,
    };
    assert!(recents_hidden(&status(VaultState::Locked, Protection::Auth)));
    assert!(recents_hidden(&status(VaultState::Unlocked, Protection::Auth)));
    assert!(!recents_hidden(&status(VaultState::Unlocked, Protection::Plain)));
    assert!(!recents_hidden(&status(VaultState::Locked, Protection::Plain)));
}

#[test]
fn the_recents_answer_is_read_strictly() {
    assert_eq!(
        recents_from_wire(&json!({ "applied": true, "mode": "system" })),
        Some(RecentsAnswer { applied: true, mode: RecentsMode::System })
    );
    assert_eq!(
        recents_from_wire(&json!({ "applied": false, "mode": "unsupported" })),
        Some(RecentsAnswer { applied: false, mode: RecentsMode::Unsupported })
    );
    for bad in [
        json!(null),
        json!({}),
        json!({ "applied": true }),
        json!({ "mode": "system" }),
        json!({ "applied": "yes", "mode": "system" }),
        json!({ "applied": true, "mode": "other" }),
        json!({ "applied": true, "mode": 1 }),
    ] {
        assert_eq!(recents_from_wire(&bad), None, "{bad}");
    }
    assert_eq!(recents_to_wire(true), json!({ "hidden": true }));
    assert_eq!(RecentsMode::System.as_str(), "system");
    assert_eq!(RecentsMode::Unsupported.as_str(), "unsupported");
}
