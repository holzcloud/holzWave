//! The plain functions of the commands that work on files and the grants that show them: the browsing and saving
//! commands, and the three that hand a file to the page's viewers (`file_grant`, `file_range`, `file_revoke`).
//!
//! Every command works on the files the calling person's own worker can reach (the worker runs as that person), and a
//! granted file is named by a token that only the core knows; nothing here widens that.

use holzwave_core::blocks::{Opened, Saved};
use holzwave_core::files::{Entry, Listing};

use super::wire::GrantAnswer;
use crate::ranges::CHUNK;
use crate::state::AppCore;

/// Permission to show one image, video, audio or PDF: the answer has a `token`, the media type and the size, and `url`
/// is where the host serves the file from (with byte ranges, so a video seeks and nothing is loaded whole). The host
/// says what that address is: the desktop's file scheme or the server's file route. The grant names that one file and
/// ends with [`file_revoke`].
pub fn file_grant(core: &AppCore, source: String, path: String) -> Result<GrantAnswer, String> {
    core.file_grant(&source, &path).map(|grant| GrantAnswer { url: core.host().file_url(&grant.token), grant })
}

/// `len` bytes of a granted file from `offset` (at most [`CHUNK`]), as raw bytes: what the PDF viewer reads pages with,
/// so a large document is read in pieces and never whole. Fewer bytes come back at the end of the file, none past it.
pub fn file_range(core: &AppCore, token: String, offset: u64, len: u64) -> Result<Vec<u8>, String> {
    let len = len.min(CHUNK);
    match core.read_granted(&token, offset, len) {
        Some(Ok(bytes)) => Ok(bytes),
        Some(Err(why)) => Err(why),
        None => Err("no such grant".into()),
    }
}

/// End a grant.
pub fn file_revoke(core: &AppCore, token: String) {
    core.file_revoke(&token);
}

/// The entries of a directory, folders first. Over SFTP this waits for the network.
pub fn files_list(core: &AppCore, source: String, path: String) -> Result<Listing, String> {
    core.files_list(&source, &path)
}

/// What a file is: its kind, size, and for Markdown the sanitised HTML, for small text the content.
pub fn file_open(core: &AppCore, source: String, path: String) -> Result<Opened, String> {
    core.file_open(&source, &path)
}

/// One entry, following links.
pub fn file_stat(core: &AppCore, source: String, path: String) -> Result<Entry, String> {
    core.file_stat(&source, &path)
}

/// Save the editor's text over a file (written aside and renamed over it). `expected_modified` is the change time the file
/// was opened with and `expected_digest` the SHA-256 of the bytes that were read: with a digest the check is exact (the file
/// is read again), without one the change time is compared; a file that changed meanwhile is not overwritten (`null` for
/// both overwrites). The answer is the new change time and the digest of what was written.
pub fn file_save(
    core: &AppCore,
    source: String,
    path: String,
    text: String,
    expected_modified: Option<i64>,
    expected_digest: Option<String>,
) -> Result<Saved, String> {
    core.file_save(&source, &path, &text, expected_modified, expected_digest.as_deref())
}
