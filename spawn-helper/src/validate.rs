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

/// Checks the worker program at helper start and returns its fully resolved path. The file must be a regular file, and
/// neither it nor any directory above it may be writable by the helper's user (an owner can always change the mode) or
/// by a group the helper belongs to, nor by everybody. Otherwise a compromised supervisor could substitute the program
/// the helper starts under another user's id.
#[cfg(unix)]
pub fn worker_file_is_protected(path: &Path, own_uid: u32, own_gids: &[u32]) -> Result<PathBuf, String> {
    use std::os::unix::fs::MetadataExt;

    if !path.is_absolute() {
        return Err("the worker path is not absolute".to_owned());
    }
    let real = std::fs::canonicalize(path).map_err(|_| "the worker file cannot be found".to_owned())?;
    let meta = std::fs::metadata(&real).map_err(|_| "the worker file cannot be read".to_owned())?;
    if !meta.is_file() {
        return Err("the worker path is not a regular file".to_owned());
    }
    for entry in real.ancestors() {
        let meta = std::fs::metadata(entry).map_err(|_| "a directory above the worker file cannot be read".to_owned())?;
        let mode = meta.mode();
        let what = if entry == real { "the worker file" } else { "a directory above the worker file" };
        if meta.uid() == own_uid {
            return Err(format!("{what} ({}) belongs to the helper's user, who could change its mode", entry.display()));
        }
        if mode & 0o002 != 0 {
            return Err(format!("{what} ({}) can be written by everybody", entry.display()));
        }
        if mode & 0o020 != 0 && own_gids.contains(&meta.gid()) {
            return Err(format!("{what} ({}) can be written by a group of the helper", entry.display()));
        }
    }
    Ok(real)
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

    #[cfg(unix)]
    mod worker_file {
        use super::super::*;
        use std::os::unix::fs::PermissionsExt;

        // Under the build directory and not under /tmp: the check refuses a directory that everybody can write to, which
        // /tmp is on Linux.
        fn scratch(tag: &str) -> PathBuf {
            let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("target").join("worker-file-tests");
            let dir = base.join(format!("{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for made in [&dir, &base] {
                std::fs::set_permissions(made, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            dir
        }

        fn file_in(dir: &Path) -> PathBuf {
            let f = dir.join("worker");
            std::fs::write(&f, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
            f
        }

        // The test user owns these files, so a helper that is somebody else is what makes the chain acceptable.
        const OTHER_UID: u32 = 0x7ffffff0;
        const OTHER_GID: u32 = 0x7ffffff1;

        #[test]
        fn accepts_a_chain_the_helper_cannot_write() {
            let dir = scratch("ok");
            let f = file_in(&dir);
            let got = worker_file_is_protected(&f, OTHER_UID, &[OTHER_GID]).expect("accepted");
            assert!(got.ends_with("worker"));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn refuses_a_file_that_belongs_to_the_helper() {
            let dir = scratch("own");
            let f = file_in(&dir);
            use std::os::unix::fs::MetadataExt;
            let me = std::fs::metadata(&f).unwrap().uid();
            assert!(worker_file_is_protected(&f, me, &[]).is_err());
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn refuses_a_world_writable_directory_above() {
            let dir = scratch("world");
            let f = file_in(&dir);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
            assert!(worker_file_is_protected(&f, OTHER_UID, &[OTHER_GID]).is_err());
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn refuses_a_group_writable_directory_of_a_group_of_the_helper() {
            let dir = scratch("group");
            let f = file_in(&dir);
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).unwrap();
            use std::os::unix::fs::MetadataExt;
            let gid = std::fs::metadata(&dir).unwrap().gid();
            assert!(worker_file_is_protected(&f, OTHER_UID, &[gid]).is_err(), "the helper is in the group");
            assert!(worker_file_is_protected(&f, OTHER_UID, &[OTHER_GID]).is_ok(), "the helper is not in the group");
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn refuses_a_directory_and_a_missing_file_and_a_relative_path() {
            let dir = scratch("kinds");
            assert!(worker_file_is_protected(&dir, OTHER_UID, &[]).is_err(), "a directory");
            assert!(worker_file_is_protected(&dir.join("nope"), OTHER_UID, &[]).is_err(), "missing");
            assert!(worker_file_is_protected(Path::new("worker"), OTHER_UID, &[]).is_err(), "relative");
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn resolves_a_link_and_checks_the_real_file() {
            let dir = scratch("link");
            let f = file_in(&dir);
            let link = dir.join("link");
            std::os::unix::fs::symlink(&f, &link).unwrap();
            let got = worker_file_is_protected(&link, OTHER_UID, &[OTHER_GID]).expect("accepted");
            assert!(got.ends_with("worker"), "the answer is the real path, not the link");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
