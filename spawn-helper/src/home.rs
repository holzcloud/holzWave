//! The home directories of people, created and checked through one directory descriptor.
//!
//! The helper opens the home base once at start and keeps the descriptor. Every later operation names an entry relative
//! to that descriptor (`mkdirat`, `openat` with no-follow, `statat` without following links, `unlinkat`), so a path is
//! never resolved a second time and a link planted where a home should be is never followed.

use crate::caps::Raised;
use crate::validate::home_name;
use rustix::fd::OwnedFd;
use rustix::fs::{AtFlags, CWD, FileType, Gid, Mode, OFlags, Uid, fchmod, fchown, fstat, mkdirat, openat, statat, unlinkat};
use rustix::io::Errno;
use std::path::Path;

/// The state of the entry at a person's home path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Existing {
    /// There is no entry.
    Missing,
    /// A real directory, owned by that person, without any group or other permission.
    Valid,
    /// Anything else: a link, a file, a directory of someone else, a directory others can enter.
    Invalid,
}

/// Why a home could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeError {
    /// The entry is not an acceptable home and was left as it was.
    Refused,
    /// A system call failed.
    Failed(Errno),
}

impl From<Errno> for HomeError {
    fn from(e: Errno) -> HomeError {
        HomeError::Failed(e)
    }
}

/// The directory that holds the homes, as a descriptor.
pub struct HomeBase {
    fd: OwnedFd,
}

impl HomeBase {
    /// Opens the base and checks it: a directory owned by the helper's user that nobody else can write to. If another
    /// user could write there, that user could replace a home with a link between two operations of the helper.
    pub fn open(path: &Path, own_uid: u32) -> Result<HomeBase, String> {
        let fd = openat(CWD, path, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())
            .map_err(|e| format!("the home base {} cannot be opened: {e}", path.display()))?;
        let st = fstat(&fd).map_err(|e| format!("the home base cannot be inspected: {e}"))?;
        if st.st_uid != own_uid {
            return Err(format!("the home base {} does not belong to the helper's user", path.display()));
        }
        if st.st_mode & 0o022 != 0 {
            return Err(format!("the home base {} can be written by its group or by others", path.display()));
        }
        Ok(HomeBase { fd })
    }

    /// What is at the home path of `uid`, looked at without following a link and without opening it (the helper cannot
    /// enter a private home of another user, and does not need to).
    pub fn inspect(&self, uid: u32) -> Result<Existing, Errno> {
        match statat(&self.fd, home_name(uid), AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => Ok(Existing::Missing),
            Err(e) => Err(e),
            Ok(st) => {
                let directory = FileType::from_raw_mode(st.st_mode as _) == FileType::Directory;
                let private = st.st_mode & 0o077 == 0;
                if directory && private && st.st_uid == uid {
                    Ok(Existing::Valid)
                } else {
                    Ok(Existing::Invalid)
                }
            }
        }
    }

    /// Makes sure `uid` has a private home. An acceptable existing one is kept as it is. A missing one is created with
    /// `mkdirat` (mode 0700), opened without following links, set to 0700 through the descriptor and only then given to
    /// the person, because after the change of owner the helper has no right to change the mode any more. If the change
    /// of owner fails the directory is removed again, so that nothing is left behind.
    pub fn ensure(&self, uid: u32, gid: u32) -> Result<(), HomeError> {
        match self.inspect(uid)? {
            Existing::Valid => return Ok(()),
            Existing::Invalid => return Err(HomeError::Refused),
            Existing::Missing => {}
        }
        let name = home_name(uid);
        match mkdirat(&self.fd, &name, Mode::RWXU) {
            Ok(()) => {}
            // Somebody made it between the look and the call: do not adopt it.
            Err(Errno::EXIST) => return Err(HomeError::Refused),
            Err(e) => return Err(e.into()),
        }
        let made = self.take_over(&name, uid, gid);
        if made.is_err() {
            let _ = unlinkat(&self.fd, &name, AtFlags::REMOVEDIR);
        }
        made
    }

    fn take_over(&self, name: &str, uid: u32, gid: u32) -> Result<(), HomeError> {
        let dir = openat(
            &self.fd,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let st = fstat(&dir)?;
        // What was just made is a directory of ours that no one else can enter, or something went wrong.
        if FileType::from_raw_mode(st.st_mode as _) != FileType::Directory || st.st_mode & 0o077 != 0 {
            return Err(HomeError::Refused);
        }
        fchmod(&dir, Mode::RWXU)?;
        let _raised = Raised::new()?;
        fchown(&dir, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))?;
        Ok(())
    }

    /// Removes the (empty) home directory of `uid`.
    pub fn remove_empty(&self, uid: u32) -> Result<(), Errno> {
        unlinkat(&self.fd, home_name(uid), AtFlags::REMOVEDIR)
    }
}
