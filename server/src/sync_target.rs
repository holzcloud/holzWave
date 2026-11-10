//! The server as a sync target: what a path may be, what a body must look like, and the one write that stores it.
//!
//! The server stores the files the sync engine of a device already produced: sealed blobs it can not read, and one small JSON
//! file that says how the key is derived. So everything here is bookkeeping:
//!
//! * **A path has three shapes** and no more: `meta.json`, `files/<name>` and `backups/<name>`, where `<name>` passes the same
//!   check the WebDAV transport uses ([`safe_segment`]). The path is never decoded: a `%` is not a character of a name, so an
//!   encoded slash or dot is simply not a path.
//! * **A blob is checked for its structure and never opened.** The header of the engine's format says the file is one of ours
//!   and which version it has; that is all the server asks ([`stamp_of`]). It holds no key and learns no name of a setting.
//! * **`meta.json` is written once.** It carries the salt and the key check of the person's sync set; whoever writes it first
//!   decides, and a second write is a lost race (the engine reads the winner's file).
//! * **The write is one step.** The condition, the bounds, the pruning of old backups and the bookkeeping run in the store's
//!   one immediate transaction ([`Tx::blob_put`]); this module only decides what is a legal request and names the refusal.
//!
//! Nothing here knows the HTTP types and nothing here logs: a path, a name and a byte of a body never leave this module in a
//! text.

use std::fmt;

use holzwave_core::sync::crypto::{MAX_BLOB, stamp_of};
use holzwave_core::sync::webdav::safe_segment;

use crate::store::{AccountId, BlobBounds, Cond, PutOutcome, StoreError, Tx};

/// The name of the one file that is not in a folder.
pub const META: &str = "meta.json";

/// The most bytes `meta.json` may have. The file the engine writes is 269 bytes (the format number, three
/// numbers of the key derivation, a 32 character salt and the sealed check text in hex); the limit leaves about fifteen times
/// that, and no more, so the file cannot be used to keep anything else.
pub const META_MAX_BYTES: usize = 4 * 1024;

/// The longest value of an `If-Match` header the server looks at, in bytes. The server's own entity tags are 64 characters.
const MAX_ETAG: usize = 128;

const MIB: u64 = 1024 * 1024;

/// A folder of the sync set, as the listing and `ensure` name it: `-` is the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    /// The root: `meta.json`.
    Root,
    /// The encrypted files.
    Files,
    /// The versions that lost a conflict.
    Backups,
}

impl Folder {
    /// The folder a route word names, or `None` for anything else.
    pub fn parse(word: &str) -> Option<Folder> {
        match word {
            "-" => Some(Folder::Root),
            "files" => Some(Folder::Files),
            "backups" => Some(Folder::Backups),
            _ => None,
        }
    }

    /// The prefix its blobs have in the store: nothing for the root, else the folder and a slash.
    pub fn prefix(self) -> &'static str {
        match self {
            Folder::Root => "",
            Folder::Files => "files/",
            Folder::Backups => "backups/",
        }
    }
}

/// The path of a blob, in one of the three shapes.
#[derive(Clone, PartialEq, Eq)]
pub enum BlobPath {
    /// `meta.json`.
    Meta,
    /// `files/<name>`.
    File(String),
    /// `backups/<name>`.
    Backup(String),
}

impl fmt::Debug for BlobPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A name is not printed: it says which setting a person has.
        f.write_str(match self {
            BlobPath::Meta => "BlobPath::Meta",
            BlobPath::File(_) => "BlobPath::File(..)",
            BlobPath::Backup(_) => "BlobPath::Backup(..)",
        })
    }
}

impl BlobPath {
    /// The path the text names, or `None` when it is not exactly one of the three shapes with a safe name.
    pub fn parse(text: &str) -> Option<BlobPath> {
        if text == META {
            return Some(BlobPath::Meta);
        }
        let (folder, name) = text.split_once('/')?;
        if !safe_segment(name) {
            return None;
        }
        match folder {
            "files" => Some(BlobPath::File(name.to_owned())),
            "backups" => Some(BlobPath::Backup(name.to_owned())),
            _ => None,
        }
    }

    /// The key the store keeps the blob under.
    pub fn key(&self) -> String {
        match self {
            BlobPath::Meta => META.to_owned(),
            BlobPath::File(name) => format!("files/{name}"),
            BlobPath::Backup(name) => format!("backups/{name}"),
        }
    }

    /// The folder the blob is listed in.
    pub fn folder(&self) -> Folder {
        match self {
            BlobPath::Meta => Folder::Root,
            BlobPath::File(_) => Folder::Files,
            BlobPath::Backup(_) => Folder::Backups,
        }
    }

    /// The name inside its folder.
    fn name(&self) -> &str {
        match self {
            BlobPath::Meta => META,
            BlobPath::File(name) | BlobPath::Backup(name) => name,
        }
    }

    /// The most bytes a body for this path may have, given the person's bound for a blob.
    pub fn body_cap(&self, bounds: &Bounds) -> usize {
        match self {
            BlobPath::Meta => META_MAX_BYTES,
            _ => bounds.max_blob_bytes,
        }
    }
}

/// What one person may keep on the server and how large one file may be. The operator sets these (`sync.*` in the
/// configuration); every one lies inside the bounds of its key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// The most bytes all of a person's files hold together.
    pub quota_bytes: u64,
    /// The most files a person has, `meta.json` and the backups included.
    pub max_files: usize,
    /// How many of the newest backups stay.
    pub backups_keep: usize,
    /// The most bytes one blob may have; never above the 8 MiB of the format.
    pub max_blob_bytes: usize,
}

impl Bounds {
    /// The defaults of the four keys.
    pub const DEFAULT: Bounds = Bounds {
        quota_bytes: 64 * MIB,
        max_files: 512,
        backups_keep: 20,
        max_blob_bytes: MAX_BLOB,
    };

    /// The numbers the store does its arithmetic with.
    fn store(&self) -> BlobBounds {
        BlobBounds { quota_bytes: self.quota_bytes, max_files: self.max_files, backups_keep: self.backups_keep }
    }
}

impl Default for Bounds {
    fn default() -> Bounds {
        Bounds::DEFAULT
    }
}

/// The condition of a write, read from the two headers that can carry one. `if_none_match` is the value of `If-None-Match` and
/// `if_match` the value of `If-Match`, each `None` when the header is not there. `None` is returned for a request that cannot
/// be understood: both headers, a value other than `*` for `If-None-Match`, an `If-Match` that is not one quoted tag.
pub fn condition(if_none_match: Option<&str>, if_match: Option<&str>) -> Option<Cond> {
    match (if_none_match, if_match) {
        (None, None) => Some(Cond::Any),
        (Some("*"), None) => Some(Cond::Absent),
        (None, Some(tag)) => {
            let inner = tag.strip_prefix('"')?.strip_suffix('"')?;
            let plain = !inner.is_empty() && inner.len() <= MAX_ETAG && inner.bytes().all(|b| b.is_ascii_graphic() && b != b'"');
            plain.then(|| Cond::Match(inner.to_owned()))
        }
        _ => None,
    }
}

/// Why a write was refused. Each is a different answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The request is not one the contract has: the condition does not fit the file, or the body is not the structure.
    BadRequest,
    /// The body is larger than the file may be.
    TooLarge,
    /// The condition did not hold.
    PreconditionFailed,
    /// The write would take the person over their space or their number of files.
    OverQuota,
}

/// What a write came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written {
    /// A file that was not there is stored; this is its tag.
    Created(String),
    /// A file that was there is replaced; this is its tag.
    Replaced(String),
    /// Nothing was stored.
    Refused(Refusal),
}

/// Whether `body` is a JSON object, which is all `meta.json` has to be. The size is judged by the caller.
fn is_object(body: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(body).is_ok()
}

/// Writes `body` at `path` for `account` if the request is legal and the condition holds, in the transaction `tx`.
///
/// The checks run in this order, and the first that fails names the refusal: the condition fits the file (`meta.json` takes
/// only "must not exist"), the body is not over the file's cap, the body has the structure of the file, and then the store's
/// step (condition, bounds, pruning). Nothing is stored for a refusal.
pub fn put(
    tx: &Tx<'_>,
    account: AccountId,
    path: &BlobPath,
    body: &[u8],
    cond: &Cond,
    bounds: &Bounds,
    now: i64,
) -> Result<Written, StoreError> {
    if *path == BlobPath::Meta && *cond != Cond::Absent {
        return Ok(Written::Refused(Refusal::BadRequest));
    }
    if body.len() > path.body_cap(bounds) {
        return Ok(Written::Refused(Refusal::TooLarge));
    }
    let shaped = match path {
        BlobPath::Meta => is_object(body),
        _ => stamp_of(body).is_ok(),
    };
    if !shaped {
        return Ok(Written::Refused(Refusal::BadRequest));
    }
    // Whether the write makes a file or replaces one: a condition that holds says it, and with none the listing of the
    // folder (names and tags, no bodies) does.
    let existed = match cond {
        Cond::Absent => false,
        Cond::Match(_) => true,
        Cond::Any => tx.blob_list(account, path.folder().prefix())?.iter().any(|entry| entry.name == path.name()),
    };
    Ok(match tx.blob_put(account, &path.key(), body, cond, &bounds.store(), now)? {
        PutOutcome::Stored(etag) if existed => Written::Replaced(etag),
        PutOutcome::Stored(etag) => Written::Created(etag),
        PutOutcome::PreconditionFailed => Written::Refused(Refusal::PreconditionFailed),
        PutOutcome::OverQuota | PutOutcome::TooManyFiles => Written::Refused(Refusal::OverQuota),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_has_three_shapes_and_a_safe_name() {
        assert_eq!(BlobPath::parse("meta.json"), Some(BlobPath::Meta));
        assert_eq!(BlobPath::parse("files/a.json.hwx"), Some(BlobPath::File("a.json.hwx".into())));
        assert_eq!(BlobPath::parse("backups/a-1_b.hwx"), Some(BlobPath::Backup("a-1_b.hwx".into())));
        let long = format!("files/{}", "a".repeat(129));
        let edge = format!("files/{}", "a".repeat(128));
        assert_eq!(BlobPath::parse(&long), None);
        assert!(BlobPath::parse(&edge).is_some());
        for bad in [
            "", "files", "files/", "/files/a", "files//a", "files/a/b", "files/..", "files/.", "files/a%2Fb", "files%2Fa",
            "FILES/a", "other/a", "meta.json/x", "meta.json.", "Meta.json", "files/a b", "files/\u{e4}", "backups/", "../meta.json",
        ] {
            assert_eq!(BlobPath::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_folder_word_names_one_of_three() {
        assert_eq!(Folder::parse("-"), Some(Folder::Root));
        assert_eq!(Folder::parse("files"), Some(Folder::Files));
        assert_eq!(Folder::parse("backups"), Some(Folder::Backups));
        for bad in ["", "meta.json", "Files", "files/", "..", "%66iles", "a"] {
            assert_eq!(Folder::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_condition_comes_from_one_header_or_none() {
        assert_eq!(condition(None, None), Some(Cond::Any));
        assert_eq!(condition(Some("*"), None), Some(Cond::Absent));
        assert_eq!(condition(None, Some("\"abc\"")), Some(Cond::Match("abc".into())));
        for bad in [(Some("*"), Some("\"a\"")), (Some("\"a\""), None), (Some(""), None), (None, Some("abc")), (None, Some("\"\"")), (None, Some("W/\"a\"")), (None, Some("*"))] {
            assert_eq!(condition(bad.0, bad.1), None, "{bad:?}");
        }
        let long = format!("\"{}\"", "a".repeat(MAX_ETAG + 1));
        assert_eq!(condition(None, Some(&long)), None);
    }

    #[test]
    fn a_path_prints_no_name() {
        assert!(!format!("{:?}", BlobPath::File("secret-name".into())).contains("secret"));
    }
}
