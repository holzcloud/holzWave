//! The user, mode and path rules of the helper, through the public interface.

use holzwave_spawn_helper::protocol::Mode;
use holzwave_spawn_helper::validate::{FLOOR, Range, home_name, home_path, uid_in_range};
use std::path::Path;

const OWN: u32 = 10_001;

fn default_range() -> Range {
    Range::new(20_000, 29_999, OWN).expect("the default range")
}

#[test]
fn every_user_id_near_zero_and_around_the_service_is_refused() {
    let range = default_range();
    for uid in 0..20_000u32 {
        assert!(!uid_in_range(uid, &range, OWN), "{uid} is below the range");
    }
    for uid in 30_000..70_000u32 {
        assert!(!uid_in_range(uid, &range, OWN), "{uid} is above the range");
    }
    for uid in [u32::MAX, u32::MAX - 1, u32::MAX - 2, 1 << 31, (1 << 31) - 1, 65_534, 65_535, 65_536] {
        assert!(!uid_in_range(uid, &range, OWN), "{uid}");
    }
}

#[test]
fn every_user_id_of_the_range_is_accepted_and_only_those() {
    let range = default_range();
    let accepted = (0..40_000u32).filter(|uid| uid_in_range(*uid, &range, OWN)).count();
    assert_eq!(accepted, 10_000);
    assert!(uid_in_range(20_000, &range, OWN) && uid_in_range(29_999, &range, OWN));
    assert!(!uid_in_range(19_999, &range, OWN) && !uid_in_range(30_000, &range, OWN));
}

#[test]
fn the_floor_holds_for_every_range_that_can_be_built() {
    for min in 0..FLOOR {
        assert!(Range::new(min, 29_999, OWN).is_err(), "min {min}");
    }
    // The lowest range there is still never contains a user below the floor.
    let low = Range::new(FLOOR, FLOOR + 5, OWN).expect("a range at the floor");
    for uid in 0..FLOOR {
        assert!(!uid_in_range(uid, &low, OWN), "{uid}");
    }
}

#[test]
fn the_service_user_is_never_a_person() {
    // The helper's own user is refused even when it sits inside what the range would otherwise allow.
    let range = Range::new(20_000, 29_999, OWN).expect("range");
    assert!(!uid_in_range(OWN, &range, OWN));
    let wide = Range::new(1000, 9_999, OWN).expect("range");
    assert!(uid_in_range(1000, &wide, OWN));
    assert!(!uid_in_range(1000, &wide, 1000), "a helper that runs as 1000 does not serve 1000");
    assert!(Range::new(1000, 20_000, OWN).is_err(), "a range around the service user cannot be built");
}

#[test]
fn the_three_modes_and_no_other() {
    for (word, mode) in [("serve", Mode::Serve), ("wipe_secrets", Mode::WipeSecrets), ("wipe_home", Mode::WipeHome)] {
        assert_eq!(Mode::parse(word), Some(mode));
        assert_eq!(mode.as_str(), word, "the word on the worker's command line is the word on the wire");
    }
    for word in ["", "Serve", "SERVE", "serve ", " serve", "wipe", "wipe_all", "shell", "exec", "root", "serve\0", "../serve"] {
        assert_eq!(Mode::parse(word), None, "{word:?}");
    }
}

#[test]
fn the_home_path_never_leaves_the_base() {
    let base = Path::new("/data/homes");
    let check = |uid: u32| {
        let path = home_path(base, uid);
        assert_eq!(path.parent(), Some(base), "{uid}");
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(uid.to_string().as_str()));
        assert_eq!(home_name(uid), uid.to_string());
    };
    for uid in 0..100_000u32 {
        check(uid);
    }
    for uid in (0..64).map(|bit| (1u64 << bit) as u32).chain([u32::MAX, u32::MAX - 1, 0x7fff_ffff, 0x8000_0000]) {
        check(uid);
    }
}
