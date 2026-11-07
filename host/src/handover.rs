//! Handing one file at a time between the phone's document dialog and the shell's own folder.
//!
//! On Android a shell has no access to a folder the person chose in the system's dialog, so a file crosses by a copy: into
//! `<home>/handover/` (a document the person picked), or out of the shell's folder (to a place the person names in the
//! dialog). There is no folder mirror and no wide storage permission. The dialog itself is the seam
//! [`FilePicker`](crate::seams::FilePicker); everything else is here, in plain `std`, and is proved on the desktop with a
//! fake picker.
//!
//! The host decides every name and path. A provider's display name is another app's text: [`clean_name`] reduces it to one
//! safe file name, [`unique_create`] never overwrites, and the copy out accepts only a regular file that stays inside the
//! shell's folder ([`resolve_source`]). A file larger than [`MAX_HANDOVER_BYTES`] is refused while it is copied, a
//! half-copied staging file is removed on every exit, and only one hand-over runs at a time.
//!
//! Nothing here logs: a name or a path never leaves the answer it belongs to.

use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

use crate::seams::{FilePicker, PickError};

/// The largest file that is handed over, in either direction: 256 MiB. The picker enforces it while it copies and the host
/// checks it again on what arrived.
pub const MAX_HANDOVER_BYTES: u64 = 256 * 1024 * 1024;

/// The folder of the shell's home that receives imported files.
pub const FOLDER: &str = "handover";

/// The most bytes in one file name (the limit of the file systems of Android).
pub const MAX_NAME_BYTES: usize = 255;

/// The most bytes after the dot that still count as an extension, so that a long name keeps `.pdf` and a long run of
/// characters after a dot is cut like any other text.
const MAX_EXTENSION_BYTES: usize = 16;

/// The most bytes of the path text of a copy out.
const MAX_PATH_TEXT: usize = 4096;

/// The name a text that is nothing but dots, spaces and separators becomes.
const FALLBACK_NAME: &str = "file";

/// The start of the name of a staging file.
const STAGING_PREFIX: &str = ".incoming-";

/// How many numbers are tried before a name is given up.
const MAX_NUMBERS: u32 = 10_000;

/// What a hand-over came to. The commands turn it into the page's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The file crossed. `name` is the cleaned name of the file that was chosen, `shown` the text the person reads for its
    /// place (`~/handover/<name>`, with the number added when the name was taken, for a copy in; `~/<path>` for a copy out),
    /// `renamed` is true when a number was added to the name.
    Done { name: String, size: u64, shown: String, renamed: bool },
    /// The person left the dialog; nothing changed.
    Cancelled,
    /// The file is larger than [`MAX_HANDOVER_BYTES`]; the size is known when the provider or the file said it.
    TooLarge { name: String, size: Option<u64> },
    /// Anything else. No text: a failure here may carry a path or a provider's words.
    Failed,
}

fn is_hostile_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            // zero-width characters and the direction marks, the line and paragraph separators, the embeddings and
            // overrides (the bidi tricks that make `gpj.exe` read as `exe.jpg`), the invisible operators, the isolates
            '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
        )
}

fn is_edge(c: char) -> bool {
    c == '.' || c == ' '
}

/// Cut `text` to at most `max` bytes at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The part of `name` that is its extension (the dot included) and the stem before it. A dot at the start is no
/// extension, and neither is a run of more than [`MAX_EXTENSION_BYTES`] bytes after the last dot.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(at) if at > 0 && name.len() - at - 1 <= MAX_EXTENSION_BYTES => (&name[..at], &name[at..]),
        _ => (name, ""),
    }
}

/// Reduce a name that another app chose to one safe file name.
///
/// Separators (`/` and `\`) split the text, the pieces `.` and `..` and the pieces of only dots and spaces are dropped, and
/// the rest is joined with `_`, so `../../etc/passwd` becomes `etc_passwd`. A control character, a zero-width or direction
/// character is replaced by `_`. Dots and spaces at either end go. A text that leaves nothing is `file`. At most 255 bytes
/// stay, cut at a character boundary and keeping an extension of up to 16 bytes after the dot. The case is never changed.
pub fn clean_name(raw: &str) -> String {
    let replaced: String = raw.chars().map(|c| if is_hostile_char(c) { '_' } else { c }).collect();
    let pieces: Vec<&str> =
        replaced.split(['/', '\\']).map(|piece| piece.trim_matches(is_edge)).filter(|piece| !piece.is_empty()).collect();
    let joined = pieces.join("_");
    if joined.is_empty() {
        return FALLBACK_NAME.to_owned();
    }
    if joined.len() <= MAX_NAME_BYTES {
        return joined;
    }
    let (stem, extension) = split_extension(&joined);
    let stem = cut(stem, MAX_NAME_BYTES - extension.len()).trim_end_matches(is_edge);
    format!("{stem}{extension}")
}

/// `name` with `-n` before its extension (`report.pdf` is `report-2.pdf`), still within [`MAX_NAME_BYTES`].
fn numbered(name: &str, n: u32) -> String {
    let (stem, extension) = split_extension(name);
    let suffix = format!("-{n}");
    let room = MAX_NAME_BYTES - extension.len() - suffix.len();
    format!("{}{suffix}{extension}", cut(stem, room))
}

/// Create a file that must not exist yet, readable by the app user only. `create_new` is one system call that fails when
/// the name is taken (also when it is a dangling link), so a race can neither overwrite a file nor follow a link.
fn create_new_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

/// Create the first free name for `name` in `dir`: `name`, then `stem-2.ext`, `stem-3.ext` and so on. Returns the open file
/// and the name it got. An existing file is never touched.
pub fn unique_create(dir: &Path, name: &str) -> io::Result<(File, String)> {
    for n in 1..=MAX_NUMBERS {
        let candidate = if n == 1 { name.to_owned() } else { numbered(name, n) };
        match create_new_private(&dir.join(&candidate)) {
            Ok(file) => return Ok((file, candidate)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("no free name"))
}

/// Which file a path names, as the system tells it: the device and the number of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity(Option<(u64, u64)>);

impl Identity {
    fn of(meta: &fs::Metadata) -> Identity {
        #[cfg(unix)]
        {
            Identity(Some((meta.dev(), meta.ino())))
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            Identity(None)
        }
    }
}

/// Open a regular file for reading: it must not be a link, and the file that was opened must be the one that was looked at
/// (a name swapped between the look and the open is caught). Returns the file and its size.
///
/// This stands for a no-follow open: the flag's number differs by processor and the standard library has no name for it, so
/// the check is made on what the open returned instead.
pub fn open_regular(path: &Path) -> io::Result<(File, u64)> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    let file = File::open(path)?;
    let after = file.metadata()?;
    if !after.is_file() || Identity::of(&before) != Identity::of(&after) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "the file was replaced"));
    }
    Ok((file, after.len()))
}

/// Why a path text of a copy out is refused. The answer to the page is `failed` for every one of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Empty, too long, a control character, `..`, an absolute path.
    Text,
    /// The file is not there, or the folder cannot be read.
    Missing,
    /// It resolves to somewhere outside the shell's folder (a link that leaves it).
    Outside,
    /// A directory, a device, a socket: not a regular file.
    NotAFile,
}

/// A file of the shell's folder that a copy out may hand to the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The real path, with no link in it, inside the real home.
    pub path: PathBuf,
    /// The file's own name, the start of what the dialog suggests.
    pub name: String,
    /// `~/` and the path from the home, as the person reads it.
    pub shown: String,
    pub size: u64,
    pub identity: Identity,
}

/// Find the file a person typed for a copy out and prove it is a regular file inside `home`.
///
/// The text is a path from the home (`report.pdf`, `notes/a.txt`, or with the `~/` that the confirmation lines show). An
/// absolute path, a `..`, a control character, an empty text and one over 4096 bytes are refused before the file system is
/// asked. The path is then resolved through every link, and the real path must lie inside the real home; a link inside the
/// folder that points to another place in the folder is fine, one that leaves it is not.
pub fn resolve_source(home: &Path, text: &str) -> Result<Source, Refusal> {
    if text.is_empty() || text.len() > MAX_PATH_TEXT || text.chars().any(char::is_control) {
        return Err(Refusal::Text);
    }
    let relative = text.strip_prefix("~/").unwrap_or(text);
    let mut parts = PathBuf::new();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => parts.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return Err(Refusal::Text),
        }
    }
    if parts.as_os_str().is_empty() {
        return Err(Refusal::Text);
    }
    let real_home = fs::canonicalize(home).map_err(|_| Refusal::Missing)?;
    let real = fs::canonicalize(real_home.join(&parts)).map_err(|_| Refusal::Missing)?;
    let inside = real.strip_prefix(&real_home).map_err(|_| Refusal::Outside)?;
    if inside.as_os_str().is_empty() {
        return Err(Refusal::Outside);
    }
    let meta = fs::symlink_metadata(&real).map_err(|_| Refusal::Missing)?;
    if !meta.is_file() {
        return Err(Refusal::NotAFile);
    }
    let shown_path: Vec<String> = inside.components().map(|part| part.as_os_str().to_string_lossy().into_owned()).collect();
    let name = real.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Source {
        path: real,
        name,
        shown: format!("~/{}", shown_path.join("/")),
        size: meta.len(),
        identity: Identity::of(&meta),
    })
}

/// Open the file that [`resolve_source`] found, for the moment just before the dialog: it must still be the same regular
/// file at the same real path (a file replaced by a link or by another file in between is refused).
pub fn reopen_source(source: &Source) -> io::Result<File> {
    let (file, _) = open_regular(&source.path)?;
    let same = file.metadata().is_ok_and(|meta| Identity::of(&meta) == source.identity)
        && fs::canonicalize(&source.path).is_ok_and(|real| real == source.path);
    if same { Ok(file) } else { Err(io::Error::new(io::ErrorKind::InvalidInput, "the file was replaced")) }
}

/// The shell folders that have a hand-over running. One dialog at a time: a second call finds the folder busy.
fn busy() -> &'static Mutex<BTreeSet<PathBuf>> {
    static BUSY: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
    BUSY.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// Holds the right to run a hand-over in one home until it is dropped.
struct Turn(PathBuf);

impl Turn {
    fn take(home: &Path) -> Option<Turn> {
        let mut running = busy().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        running.insert(home.to_path_buf()).then(|| Turn(home.to_path_buf()))
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        busy().lock().unwrap_or_else(|poisoned| poisoned.into_inner()).remove(&self.0);
    }
}

/// A staging file that is removed whichever way the hand-over ends.
struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// A file that is removed unless it is kept: the half of a copy that failed.
struct Partial(Option<PathBuf>);

impl Partial {
    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

/// The folder for imported files, made when it is missing (readable by the app user only). A link in its place is refused,
/// so an import never lands outside the home.
fn ensure_folder(home: &Path) -> io::Result<PathBuf> {
    let dir = home.join(FOLDER);
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(io::Error::new(io::ErrorKind::InvalidInput, "the hand-over folder is not a folder")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(&dir)?;
        }
        Err(error) => return Err(error),
    }
    Ok(dir)
}

/// Remove the staging files that an earlier run left (the app was killed in a dialog). Only regular files with the
/// staging prefix; nothing else in the folder is looked at.
fn sweep_staging(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let is_staging = name.to_str().is_some_and(|name| name.starts_with(STAGING_PREFIX));
        if is_staging && entry.file_type().is_ok_and(|kind| kind.is_file()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Reserve a staging name in `dir` (private, never existing before) and return its guard.
fn new_staging(dir: &Path) -> io::Result<Staging> {
    for _ in 0..8 {
        let bytes = holzwave_core::sync::crypto::random::<8>().map_err(|_| io::Error::other("no random bytes"))?;
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = dir.join(format!("{STAGING_PREFIX}{hex}"));
        match create_new_private(&path) {
            Ok(_) => return Ok(Staging(path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("no free staging name"))
}

/// Copy at most `max` bytes and say how many came; one more than `max` means the source was larger.
fn copy_bounded(from: File, to: &mut File, max: u64) -> io::Result<u64> {
    io::copy(&mut from.take(max.saturating_add(1)), to)
}

/// Bring one file from the dialog into `<home>/handover/`.
///
/// The picker writes the chosen document to a staging file in that folder, at most [`MAX_HANDOVER_BYTES`]; the host then
/// reads it back bounded, names it (cleaned, and numbered when taken) and removes the staging file. Nothing stays in the
/// folder when the dialog is cancelled or anything fails.
pub fn import(home: &Path, picker: &dyn FilePicker) -> Outcome {
    let Some(_turn) = Turn::take(home) else { return Outcome::Failed };
    let Ok(dir) = ensure_folder(home) else { return Outcome::Failed };
    sweep_staging(&dir);
    let Ok(staging) = new_staging(&dir) else { return Outcome::Failed };

    let picked = catch_unwind(AssertUnwindSafe(|| picker.pick_in(&staging.0, MAX_HANDOVER_BYTES)));
    let picked = match picked {
        Ok(Ok(picked)) => picked,
        Ok(Err(PickError::Cancelled)) => return Outcome::Cancelled,
        Ok(Err(PickError::TooLarge { name, size })) => return Outcome::TooLarge { name: clean_name(&name), size },
        Ok(Err(PickError::Failed)) | Err(_) => return Outcome::Failed,
    };
    let wanted = clean_name(&picked.name);
    if picked.size > MAX_HANDOVER_BYTES {
        return Outcome::TooLarge { name: wanted, size: Some(picked.size) };
    }

    let Ok((source, staged)) = open_regular(&staging.0) else { return Outcome::Failed };
    if staged > MAX_HANDOVER_BYTES {
        return Outcome::TooLarge { name: wanted, size: Some(staged) };
    }
    let Ok((mut target, name)) = unique_create(&dir, &wanted) else { return Outcome::Failed };
    let partial = Partial(Some(dir.join(&name)));
    match copy_bounded(source, &mut target, MAX_HANDOVER_BYTES) {
        Ok(size) if size > MAX_HANDOVER_BYTES => Outcome::TooLarge { name: wanted, size: None },
        Ok(size) => {
            partial.keep();
            Outcome::Done { shown: format!("~/{FOLDER}/{name}"), renamed: name != wanted, name: wanted, size }
        }
        Err(_) => Outcome::Failed,
    }
}

/// Hand one file of the shell's folder to the dialog that asks where to put it.
///
/// `text` is the path the person typed, from the home. It is checked by [`resolve_source`] before anything else, and the
/// picker is not called for a path that is refused. The file is opened and compared with what was found just before the
/// dialog starts.
pub fn export(home: &Path, text: &str, picker: &dyn FilePicker) -> Outcome {
    let Some(_turn) = Turn::take(home) else { return Outcome::Failed };
    let Ok(source) = resolve_source(home, text) else { return Outcome::Failed };
    let suggested = clean_name(&source.name);
    if source.size > MAX_HANDOVER_BYTES {
        return Outcome::TooLarge { name: suggested, size: Some(source.size) };
    }
    let Ok(_held) = reopen_source(&source) else { return Outcome::Failed };

    match catch_unwind(AssertUnwindSafe(|| picker.save_out(&source.path, &suggested))) {
        Ok(Ok(saved)) => Outcome::Done { name: suggested, size: saved.size, shown: source.shown, renamed: false },
        Ok(Err(PickError::Cancelled)) => Outcome::Cancelled,
        Ok(Err(PickError::TooLarge { name, size })) => Outcome::TooLarge { name: clean_name(&name), size },
        Ok(Err(PickError::Failed)) | Err(_) => Outcome::Failed,
    }
}
