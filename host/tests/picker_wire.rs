//! The wire between the Kotlin document dialogs and the host's file picker seam: what an answer of the plugin becomes, and what
//! the plugin is asked. No Android code runs here; the shapes are the ones the Kotlin side sends.

use std::path::Path;

use holzwave_host::picker_wire::{
    FAILED_TEXT, MAX_NAME_CHARS, pick_in_to_wire, picked_in_from_wire, picked_out_from_wire, save_out_to_wire,
};
use holzwave_host::seams::{PickError, PickedIn, PickedOut};
use serde_json::{Value, json};

const CANARY: &str = "HW-CANARY-9d41c7";

#[test]
fn an_open_answer_becomes_a_picked_file_a_cancel_or_a_too_large_error() {
    assert_eq!(
        picked_in_from_wire(&json!({ "kind": "done", "name": "report.pdf", "size": 1048576 })),
        Ok(PickedIn { name: "report.pdf".to_owned(), size: 1_048_576 })
    );
    assert_eq!(picked_in_from_wire(&json!({ "kind": "cancelled" })), Err(PickError::Cancelled));
    assert_eq!(
        picked_in_from_wire(&json!({ "kind": "too_large", "name": "x", "size": 300000000 })),
        Err(PickError::TooLarge { name: "x".to_owned(), size: Some(300_000_000) })
    );
    assert_eq!(
        picked_in_from_wire(&json!({ "kind": "too_large" })),
        Err(PickError::TooLarge { name: String::new(), size: None })
    );
    // The size is optional only where the provider may not say it.
    assert_eq!(
        picked_in_from_wire(&json!({ "kind": "too_large", "name": "x" })),
        Err(PickError::TooLarge { name: "x".to_owned(), size: None })
    );
    assert_eq!(
        picked_in_from_wire(&json!({ "kind": "done", "name": "empty", "size": 0 })),
        Ok(PickedIn { name: "empty".to_owned(), size: 0 })
    );
}

#[test]
fn every_malformed_answer_is_one_fixed_failure() {
    let nested = json!({ "kind": "done", "name": "a", "size": 1, "extra": { "deep": [1, 2, 3] } });
    let big_float = json!({ "kind": "done", "name": "a", "size": 1.0e300 });
    let cases: Vec<Value> = vec![
        json!({ "kind": "other" }),
        json!({ "kind": "Done", "name": "a", "size": 1 }),
        json!({ "kind": "done ", "name": "a", "size": 1 }),
        json!({ "name": "a", "size": 1 }),
        json!({ "kind": 1 }),
        json!({ "kind": null }),
        json!({ "kind": ["done"] }),
        json!({ "kind": { "done": true } }),
        json!({ "kind": "done", "name": "a", "size": -1 }),
        json!({ "kind": "done", "name": "a", "size": 1.5 }),
        json!({ "kind": "done", "name": "a", "size": 1048576.0 }),
        json!({ "kind": "done", "name": "a", "size": "12" }),
        json!({ "kind": "done", "name": "a" }),
        json!({ "kind": "done", "name": "a", "size": null }),
        json!({ "kind": "done", "name": 5, "size": 1 }),
        json!({ "kind": "done", "name": null, "size": 1 }),
        json!({ "kind": "done", "name": ["a"], "size": 1 }),
        json!({ "kind": "too_large", "name": 5, "size": 1 }),
        json!({ "kind": "too_large", "name": "a", "size": -3 }),
        json!({ "kind": "too_large", "name": "a", "size": 2.5 }),
        json!({ "kind": "cancelled", "name": "a" }),
        nested,
        big_float,
        json!({ "kind": "done", "name": "a", "size": 18446744073709551616u128.to_string() }),
        json!(null),
        json!(true),
        json!(7),
        json!("done"),
        json!([]),
        json!([{ "kind": "done", "name": "a", "size": 1 }]),
        json!({}),
    ];
    for case in cases {
        assert_eq!(picked_in_from_wire(&case), Err(PickError::Failed), "{case}");
    }
    // The largest whole number is accepted, a float above it is not.
    assert_eq!(
        picked_in_from_wire(&json!({ "kind": "done", "name": "a", "size": u64::MAX })),
        Ok(PickedIn { name: "a".to_owned(), size: u64::MAX })
    );
    assert_eq!(FAILED_TEXT, "the file dialog failed");
}

#[test]
fn the_name_is_cut_at_1024_characters_on_a_character_boundary_and_nothing_else_is_changed() {
    assert_eq!(MAX_NAME_CHARS, 1024);
    let long = "a".repeat(100_000);
    let name = picked_in_from_wire(&json!({ "kind": "done", "name": long, "size": 1 })).expect("a file").name;
    assert_eq!(name.chars().count(), 1024);
    assert!(name.chars().all(|c| c == 'a'));
    // A character of several bytes is never cut in the middle.
    let wide = "\u{e9}".repeat(5000);
    let name = picked_in_from_wire(&json!({ "kind": "done", "name": wide, "size": 1 })).expect("a file").name;
    assert_eq!(name.chars().count(), 1024);
    assert_eq!(name.len(), 2048);
    let emoji = "\u{1f600}".repeat(2000);
    let name = picked_in_from_wire(&json!({ "kind": "done", "name": emoji, "size": 1 })).expect("a file").name;
    assert_eq!(name.chars().count(), 1024);
    // Exactly the bound stays whole.
    let exact = "b".repeat(1024);
    assert_eq!(picked_in_from_wire(&json!({ "kind": "done", "name": exact, "size": 1 })).expect("a file").name, exact);
    // A NUL, a separator and a right-to-left override pass through: cleaning is the host's job, tested there.
    let odd = "a\u{0}b/c\\d\u{202e}e.pdf";
    assert_eq!(picked_in_from_wire(&json!({ "kind": "done", "name": odd, "size": 1 })).expect("a file").name, odd);
    // A missing name on a finished copy is the empty text (the host turns it into `file`).
    assert_eq!(
        picked_in_from_wire(&json!({ "kind": "done", "size": 3 })),
        Ok(PickedIn { name: String::new(), size: 3 })
    );
    // The same cap holds for the name of a too large file.
    let long = "z".repeat(5000);
    match picked_in_from_wire(&json!({ "kind": "too_large", "name": long, "size": 9 })) {
        Err(PickError::TooLarge { name, size }) => assert_eq!((name.chars().count(), size), (1024, Some(9))),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_save_answer_is_read_the_same_way() {
    assert_eq!(picked_out_from_wire(&json!({ "kind": "done", "size": 42 })), Ok(PickedOut { size: 42 }));
    assert_eq!(picked_out_from_wire(&json!({ "kind": "done", "size": 0 })), Ok(PickedOut { size: 0 }));
    assert_eq!(picked_out_from_wire(&json!({ "kind": "cancelled" })), Err(PickError::Cancelled));
    for case in [
        json!({ "kind": "too_large", "name": "x", "size": 1 }),
        json!({ "kind": "done" }),
        json!({ "kind": "done", "size": -1 }),
        json!({ "kind": "done", "size": 1.5 }),
        json!({ "kind": "done", "size": "42" }),
        json!({ "kind": "done", "size": 42, "name": "x" }),
        json!({ "kind": "done", "size": 42, "extra": { "a": 1 } }),
        json!({ "kind": "nope" }),
        json!({ "kind": 3 }),
        json!({}),
        json!(null),
        json!([1]),
        json!("done"),
    ] {
        assert_eq!(picked_out_from_wire(&case), Err(PickError::Failed), "{case}");
    }
}

#[test]
fn the_requests_carry_the_paths_and_the_limit_and_refuse_a_path_that_is_not_text() {
    assert_eq!(
        pick_in_to_wire(Path::new("/data/user/0/app/files/home/handover/.incoming-ab"), 268_435_456),
        Some(json!({ "staging_path": "/data/user/0/app/files/home/handover/.incoming-ab", "max_bytes": 268435456u64 }))
    );
    assert_eq!(
        save_out_to_wire(Path::new("/data/user/0/app/files/home/a.bin"), "a.bin"),
        Some(json!({ "source_path": "/data/user/0/app/files/home/a.bin", "suggested_name": "a.bin" }))
    );
    #[cfg(unix)]
    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let bad = Path::new(OsStr::from_bytes(b"/data/\xff\xfe"));
        assert_eq!(pick_in_to_wire(bad, 1), None);
        assert_eq!(save_out_to_wire(bad, "x"), None);
    }
}

fn hostile_values() -> Vec<Value> {
    let huge = "k".repeat(1_000_000);
    let mut deep = json!(1);
    for _ in 0..200 {
        deep = json!({ "kind": deep, "name": [1, 2] });
    }
    vec![
        json!(null),
        json!([]),
        json!([[], [[]], {}]),
        json!({}),
        json!({ "kind": huge, "name": huge, "size": huge }),
        json!({ "kind": "done", "name": huge, "size": u64::MAX }),
        json!({ "kind": "done", "name": huge, "size": 1.7976931348623157e308 }),
        json!({ "kind": "too_large", "name": huge, "size": -1e308 }),
        json!({ "kind": "done", "name": { "a": { "b": [1, 2, { "c": null }] } }, "size": [1] }),
        json!({ "kind": { "kind": "done" }, "name": "n", "size": 1 }),
        json!({ "kind": "done", "name": "n", "size": 1, "z": huge }),
        json!({ "kind": "cancelled", "z": [null] }),
        json!(1e308),
        json!(-0.0),
        json!(u64::MAX),
        json!(i64::MIN),
        deep,
        json!({ "kind": "\u{0}", "name": "\u{202e}\u{0}", "size": 0 }),
    ]
}

#[test]
fn no_function_panics_on_a_hostile_value() {
    for value in hostile_values() {
        let _ = picked_in_from_wire(&value);
        let _ = picked_out_from_wire(&value);
    }
}

#[test]
fn no_error_carries_received_text() {
    let hostile = [
        json!({ "kind": CANARY }),
        json!({ "kind": "done", "name": CANARY, "size": "x" }),
        json!({ "kind": "done", "name": CANARY }),
        json!({ "kind": "done", "name": CANARY, "size": 1, "extra": CANARY }),
        json!({ "kind": "done", "name": CANARY, "size": 1, "extra": { "a": CANARY } }),
        json!({ "kind": "too_large", "name": CANARY, "size": CANARY }),
        json!({ CANARY: CANARY }),
        json!({ "kind": "cancelled", CANARY: CANARY }),
        json!(CANARY),
        json!([CANARY]),
    ];
    for value in hostile {
        for result in [picked_in_from_wire(&value).map(|picked| picked.size), picked_out_from_wire(&value).map(|picked| picked.size)] {
            if let Err(error) = result {
                assert!(!format!("{error:?}").contains(CANARY), "Debug of {value}");
                assert!(!format!("{error:?}").contains("9d41c7"), "Debug of {value}");
            }
        }
    }
    // A too large error is the one that may hold a name, and it is the cleaned-by-the-host name of the file the person chose,
    // never received error text; it is not an error text at all but a field. A failure holds none.
    assert_eq!(picked_in_from_wire(&json!({ "kind": CANARY })), Err(PickError::Failed));
    assert_eq!(picked_out_from_wire(&json!({ "kind": CANARY })), Err(PickError::Failed));
}
