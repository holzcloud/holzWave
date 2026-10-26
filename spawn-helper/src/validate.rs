//! The rules the helper applies to every request, as pure functions with hard floors that no argument can lower.

use std::path::{Path, PathBuf};

/// No user below this number is ever used for a person, whatever the helper is configured with. Ids below 1000 belong to
/// the system (root is 0).
pub const FLOOR: u32 = 1000;

/// The default first user id of the range, used when the helper is started without `--uid-min`.
pub const DEFAULT_MIN: u32 = 20_000;

/// The default last user id of the range, used when the helper is started without `--uid-max`.
pub const DEFAULT_MAX: u32 = 29_999;

/// The inclusive range of user ids the helper may use for people. It can only be built through [`Range::new`], which
/// refuses a range below the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    min: u32,
    max: u32,
}

impl Range {
    /// A range from `min` to `max`, both included. `own_uid` is the user the helper runs as: a range that contains it
    /// would let the supervisor ask for a worker that is the service itself.
    pub fn new(min: u32, max: u32, own_uid: u32) -> Result<Range, &'static str> {
        if min < FLOOR {
            return Err("the first user id of the range is below 1000");
        }
        if min > max {
            return Err("the first user id of the range is above the last");
        }
        // u32::MAX is (uid_t)-1, the value that means "leave unchanged" to the kernel's id calls.
        if max == u32::MAX {
            return Err("the range reaches the largest user id, which the kernel reserves");
        }
        if (min..=max).contains(&own_uid) {
            return Err("the range contains the user id of the helper itself");
        }
        Ok(Range { min, max })
    }

    /// The first user id of the range.
    pub fn min(&self) -> u32 {
        self.min
    }

    /// The last user id of the range.
    pub fn max(&self) -> u32 {
        self.max
    }
}

/// Whether the helper may act for `uid`: it is inside the range, at or above the floor, not 0 and not the helper's own
/// user. Every operation that names a person goes through this function first.
pub fn uid_in_range(uid: u32, range: &Range, own_uid: u32) -> bool {
    uid >= FLOOR && uid != 0 && uid != own_uid && uid != u32::MAX && uid >= range.min && uid <= range.max
}

/// The name of the home directory of a person inside the home base: the decimal user id and nothing else.
pub fn home_name(uid: u32) -> String {
    uid.to_string()
}

/// The home path of a person: the home base joined with the decimal user id. The base was fixed at helper start; no
/// part of a request is ever a path.
pub fn home_path(base: &Path, uid: u32) -> PathBuf {
    base.join(home_name(uid))
}

/// The `USER` and `LOGNAME` value of a worker.
pub fn user_name(uid: u32) -> String {
    format!("u{uid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWN: u32 = 10_001;

    fn range() -> Range {
        Range::new(20_000, 29_999, OWN).expect("the default range is valid")
    }

    #[test]
    fn accepts_the_inside_of_the_range() {
        let r = range();
        for uid in [20_000, 20_001, 25_000, 29_998, 29_999] {
            assert!(uid_in_range(uid, &r, OWN), "{uid}");
        }
    }

    #[test]
    fn rejects_everything_at_and_beyond_the_edges() {
        let r = range();
        for uid in [0, 1, 999, 1000, 10_000, OWN, 19_999, 30_000, 65_534, u32::MAX - 1, u32::MAX] {
            assert!(!uid_in_range(uid, &r, OWN), "{uid}");
        }
    }

    #[test]
    fn the_own_uid_is_refused_even_when_a_range_would_hold_it() {
        // A range cannot be built around the own uid, but the check stands on its own.
        let r = range();
        assert!(!uid_in_range(OWN, &r, OWN));
        assert!(!uid_in_range(20_001, &r, 20_001));
    }

    #[test]
    fn the_range_refuses_what_it_must() {
        assert!(Range::new(999, 29_999, OWN).is_err(), "below the floor");
        assert!(Range::new(0, 29_999, OWN).is_err(), "root");
        assert!(Range::new(30_000, 20_000, OWN).is_err(), "min above max");
        assert!(Range::new(5_000, 15_000, OWN).is_err(), "contains the own uid");
        assert!(Range::new(20_000, u32::MAX, OWN).is_err(), "reaches the reserved id");
        assert!(Range::new(1000, 1000, OWN).is_ok(), "the floor itself, one user");
        let r = range();
        assert_eq!((r.min(), r.max()), (20_000, 29_999));
    }

    #[test]
    fn the_service_uid_is_not_a_person_whatever_the_floor_says() {
        // 1000 is the lowest a range may start; a service user at 1000 is protected by the own-uid rule.
        let r = Range::new(1000, 2000, OWN).unwrap();
        assert!(uid_in_range(1000, &r, OWN));
        assert!(!uid_in_range(1000, &r, 1000));
    }

    #[test]
    fn the_home_path_stays_inside_the_base_for_any_uid() {
        let base = Path::new("/data/homes");
        for uid in [0, 1, 999, 1000, 20_001, 29_999, 65_534, u32::MAX - 1, u32::MAX] {
            let p = home_path(base, uid);
            assert_eq!(p.parent(), Some(base), "{uid}");
            let name = p.file_name().and_then(|n| n.to_str()).expect("a name");
            assert_eq!(name, uid.to_string());
            assert!(name.bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(p.components().count(), base.components().count() + 1);
        }
    }

    #[test]
    fn the_user_name_is_u_and_the_number() {
        assert_eq!(user_name(20_001), "u20001");
    }
}
