//! The plain functions of the commands that work on files and the grants that show them: the browsing and saving
//! commands, and the three that hand a file to the page's viewers (`file_grant`, `file_range`, `file_revoke`).
//!
//! Every command works on the files the calling person's own worker can reach (the worker runs as that person), and a
//! granted file is named by a token that only the core knows; nothing here widens that.

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
