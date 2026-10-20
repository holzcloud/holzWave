//! GAM7, the command-line tool for Google Workspace administration: argument lists and parsers, and nothing else.
//!
//! This module starts no process, reads no file and holds no state: the generic engine runs the tool and hands the
//! standard output to [`Gam::view`]. Three verbs are catalogued, all reads: the sections of the tool's configuration
//! (`sections`), the domain (`domain`) and the users (`users`, optionally as JSON).
//!
//! **The tool's own configuration is never written.** GAM chooses a section with `select <section>` in front of a
//! command and saves the choice only when the word `save` follows; `config <variable> <value>` saves only with the
//! same word. A block therefore passes its section as `select <section>` for one command, per process, and no argument
//! list this module builds holds `save`, `oauth`, `create`, `update`, `delete`, `add`, `remove` or `todrive`: the
//! words are refused as a section name ([`crate::integrate::args::SectionName`]) and a final check over every list
//! refuses one that holds any of them. Isolating the configuration directory (`GAMCFGDIR`) is the context's job, not
//! this module's.
//!
//! **Output.** `print` commands write CSV, with quoted cells that hold commas, line breaks and JSON. The text is
//! written by other people into a domain's directory, so a hand-written splitter would be a parser of hostile input:
//! the established `csv` crate reads it, and this module only puts caps around it. The header is cut at 64 columns,
//! at most 2000 rows are kept (the 2001st record proves that more exist, and the model says so), a cell is cut at 4096
//! bytes, a leading byte order mark is ignored, a ragged row is padded or cut to the header and every cell is cleaned
//! of escape sequences, control and invisible characters by the view builders. Records are read one at a time.
//!
//! **Assumptions** (checked against the owner's tool in the acceptance rows, not here): the exact text of
//! `showsections` is the file line followed by the indented names with `*` on the selected one, so that reader is
//! lenient and anything else shows as plain text; `config csv_output_row_limit` is applied per process without
//! `save`, and the reader's own cap is the backstop when it is not; the member names of `info domain formatjson` are
//! the ones of the documents, and members that are not named here are dropped.

use std::ffi::OsString;

use csv::{ByteRecord, Reader, ReaderBuilder};
use serde_json::{Map, Value};

use super::args::ArgError;
use super::view::{
    clean_cell, text_fallback, text_view, KvBuilder, RowBuilder, TableBuilder, TextFormat, Tone, ViewModel, MAX_CELL,
    MAX_COLUMNS, MAX_ROWS, MAX_TEXT,
};
use super::{
    ArgKind, ArgSpec, CheckedSelection, Integration, IntegrationSpec, Output, ParseError, SelectKind, ToolId,
    Validated, Verb, VerbEffect, VersionProbe,
};

/// The most standard output this module reads; the runner's own cap is at most as large by default, and a longer
/// output is refused here too, so that nothing in the module depends on the runner for its bounds.
pub const MAX_INPUT: usize = 16 * 1024 * 1024;

/// The most rows GAM is asked for: one more than the table shows, so that the extra one proves that more exist.
pub const ROW_LIMIT: usize = MAX_ROWS + 1;

/// The user fields the `users` verb asks for, by GAM's field names.
const USER_FIELDS: &str = "primaryemail,name,suspended,orgunitpath,lastlogintime";

/// Raw bytes of a cell kept before cleaning: more than the cap, so that cleaning can only shrink the text, and bounded
/// so that a huge cell costs a bounded amount of work.
const RAW_CELL: usize = 4 * MAX_CELL;

/// The most bytes of a domain document that are parsed; a larger one shows as plain text.
const MAX_DOMAIN: usize = 1024 * 1024;

/// The words no argument list may hold anywhere (the section word included).
const FORBIDDEN: &[&str] = &["save", "oauth", "create", "update", "delete", "add", "remove", "todrive"];

const SECTIONS: Verb = Verb {
    id: "sections",
    label: "Sections",
    effect: VerbEffect::Read,
    output: Output::View,
    args: &[],
    refresh_secs: None,
    exit_is_state: false,
};

const DOMAIN: Verb = Verb {
    id: "domain",
    label: "Domain",
    effect: VerbEffect::Read,
    output: Output::View,
    args: &[],
    refresh_secs: None,
    exit_is_state: false,
};

const USERS: Verb = Verb {
    id: "users",
    label: "Users",
    effect: VerbEffect::Read,
    output: Output::View,
    args: &[ArgSpec { name: "as_json", kind: ArgKind::Flag, required: false }],
    refresh_secs: None,
    exit_is_state: false,
};

/// The compiled-in description of GAM7.
pub static SPEC: IntegrationSpec = IntegrationSpec {
    id: ToolId::Gam,
    label: "GAM7",
    binaries: &["gam"],
    windows_binaries: &["gam.exe"],
    fixed_dirs: &[
        "$HOME/bin/gam7",
        "$HOME/bin/gam",
        "$HOME/.local/bin",
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/snap/bin",
        "C:\\GAM7",
        "%LOCALAPPDATA%\\GAM7",
        "%LOCALAPPDATA%\\Microsoft\\WinGet\\Links",
    ],
    version_probe: VersionProbe { argv: &["version"], parse: parse_version },
    env_set: &[],
    selects: &[SelectKind::Section],
    verbs: &[SECTIONS, DOMAIN, USERS],
};

/// GAM7 as an [`Integration`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Gam;

/// The one value of [`Gam`].
pub static GAM: Gam = Gam;

/// The version in the answer to `gam version`: the word after `GAM` on the first line that has one that looks like a
/// version (digits and dots, such as `7.46.02`).
pub fn parse_version(text: &str) -> Option<String> {
    text.lines().take(32).find_map(|line| {
        let mut words = line.split_whitespace();
        words.find(|word| *word == "GAM")?;
        let version = words.next()?;
        let shaped = version.len() <= 32
            && version.starts_with(|c: char| c.is_ascii_digit())
            && version.contains('.')
            && version.chars().all(|c| c.is_ascii_digit() || c == '.');
        shaped.then(|| version.to_owned())
    })
}

impl Integration for Gam {
    fn spec(&self) -> &'static IntegrationSpec {
        &SPEC
    }

    fn argv(&self, verb: &Verb, args: &Validated, selection: &CheckedSelection) -> Result<Vec<OsString>, ArgError> {
        // The catalogue is closed: only a verb exactly as the spec lists it has an argument list.
        if SPEC.verb(verb.id) != Some(verb) {
            return Err(ArgError::Unknown);
        }
        let mut out: Vec<String> = Vec::new();
        let section = selection.section.as_ref().map(|section| section.as_str());
        let select = |out: &mut Vec<String>| {
            if let Some(section) = section {
                out.extend(["select".to_owned(), section.to_owned()]);
            }
        };
        let words = |out: &mut Vec<String>, list: &[&str]| out.extend(list.iter().map(|word| (*word).to_owned()));
        match verb.id {
            "sections" => words(&mut out, &["showsections"]),
            "domain" => {
                select(&mut out);
                words(&mut out, &["info", "domain", "formatjson"]);
            }
            "users" => {
                select(&mut out);
                words(&mut out, &["config", "csv_output_row_limit"]);
                out.push(ROW_LIMIT.to_string());
                words(&mut out, &["print", "users", "fields", USER_FIELDS]);
                if args.flag("as_json") == Some(true) {
                    words(&mut out, &["formatjson"]);
                }
            }
            _ => return Err(ArgError::Unknown),
        }
        // The last line of defence: no list holds a word that writes or saves, whatever built it.
        if out.iter().any(|word| FORBIDDEN.iter().any(|forbidden| word.eq_ignore_ascii_case(forbidden))) {
            return Err(ArgError::Shape);
        }
        Ok(out.into_iter().map(OsString::from).collect())
    }

    fn view(&self, verb: &Verb, stdout: &[u8]) -> Result<ViewModel, ParseError> {
        if stdout.len() > MAX_INPUT {
            return Err(ParseError::TooLarge);
        }
        match verb.id {
            "users" => users_view(stdout),
            "domain" => domain_view(stdout),
            "sections" => Ok(text_fallback(verb.label, stdout)),
            _ => Err(ParseError::Unrecognised),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// CSV.
// ---------------------------------------------------------------------------------------------------------------------

/// The header of a document: the cleaned names of the first [`MAX_COLUMNS`] columns and how many columns there are.
struct Header {
    names: Vec<String>,
    total: usize,
}

/// A cell's raw bytes as clean, bounded text.
fn cell_text(raw: &[u8]) -> String {
    let cut = raw.len() > RAW_CELL;
    let kept = &raw[..raw.len().min(RAW_CELL)];
    let mut text = clean_cell(&String::from_utf8_lossy(kept));
    if cut && !text.ends_with('\u{2026}') {
        let mut end = text.len().min(MAX_CELL - '\u{2026}'.len_utf8());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('\u{2026}');
    }
    text
}

/// A reader over `bytes` and its header. An empty document (or one of blank lines) is [`ParseError::Empty`]; a header
/// that is not text is [`ParseError::Unrecognised`].
fn open(bytes: &[u8]) -> Result<(Reader<&[u8]>, Header), ParseError> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut reader = ReaderBuilder::new().flexible(true).has_headers(true).from_reader(bytes);
    let record = reader.byte_headers().map_err(|_| ParseError::Unrecognised)?;
    let total = record.len();
    if total == 0 {
        return Err(ParseError::Empty);
    }
    let mut names = Vec::with_capacity(record.len().min(MAX_COLUMNS));
    for raw in record.iter().take(MAX_COLUMNS) {
        std::str::from_utf8(raw).map_err(|_| ParseError::Unrecognised)?;
        names.push(cell_text(raw));
    }
    Ok((reader, Header { names, total }))
}

/// Reads the records one at a time and hands each to `keep`, until the document ends or [`ROW_LIMIT`] records were
/// read. Says whether the limit was reached, which proves that more rows exist than the model keeps.
fn read_rows(reader: &mut Reader<&[u8]>, mut keep: impl FnMut(&ByteRecord)) -> Result<bool, ParseError> {
    let mut record = ByteRecord::new();
    let mut count = 0_usize;
    while reader.read_byte_record(&mut record).map_err(|_| ParseError::Unrecognised)? {
        keep(&record);
        count += 1;
        if count >= ROW_LIMIT {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A column id that no other column has: the cleaned header, `column N` when it is empty, `#2`, `#3` when repeated.
fn column_ids(names: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = Vec::with_capacity(names.len());
    for (at, name) in names.iter().enumerate() {
        let base = if name.is_empty() { format!("column {}", at + 1) } else { name.clone() };
        let mut id = base.clone();
        let mut n = 2;
        while ids.contains(&id) {
            id = format!("{base}#{n}");
            n += 1;
        }
        ids.push(id);
    }
    ids
}

/// The table of a `print users` output.
fn users_table(mut reader: Reader<&[u8]>, header: &Header) -> Result<ViewModel, ParseError> {
    let ids = column_ids(&header.names);
    let width = header.names.len();
    let key_column = header.names.iter().position(|name| name.eq_ignore_ascii_case("primaryemail"));
    let suspended = header.names.iter().position(|name| name.eq_ignore_ascii_case("suspended"));
    let mut table = TableBuilder::new("Users");
    for (id, name) in ids.iter().zip(&header.names) {
        table.column(id, name);
    }
    let mut ragged = 0_usize;
    let mut number = 0_usize;
    let more = read_rows(&mut reader, |record| {
        number += 1;
        if record.len() != header.total {
            ragged += 1;
        }
        let cells: Vec<String> = record.iter().take(width).map(cell_text).collect();
        let key = key_column.and_then(|at| cells.get(at)).filter(|key| !key.is_empty());
        let key = key.cloned().unwrap_or_else(|| format!("row {number}"));
        let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
        let mut row = RowBuilder::new(&key, &refs);
        if suspended.and_then(|at| cells.get(at)).is_some_and(|cell| cell.eq_ignore_ascii_case("true")) {
            row.tone(Tone::Dim);
        }
        table.push(row);
    })?;
    let mut notes = Vec::new();
    if header.total > width {
        notes.push(format!("First {width} of {} columns.", header.total));
    }
    if more {
        notes.push("More rows exist than are shown.".to_owned());
    }
    if ragged > 0 {
        notes.push(format!("{ragged} rows had another number of fields."));
    }
    let model = table.finish();
    Ok(if notes.is_empty() { model } else { model.with_note(&notes.join(" ")) })
}

/// The model of a `print users` output: the table of the columns GAM printed, or, when the output has the `JSON`
/// column that `formatjson` adds, the objects of that column as one JSON text. Output that is not CSV is plain text.
fn users_view(stdout: &[u8]) -> Result<ViewModel, ParseError> {
    let (reader, header) = match open(stdout) {
        Ok(opened) => opened,
        Err(ParseError::Empty) => return Ok(TableBuilder::new("Users").finish().with_note("GAM printed no rows.")),
        Err(ParseError::Unrecognised) => return Ok(text_fallback("Users", stdout)),
        Err(other) => return Err(other),
    };
    let json_column = header.names.iter().position(|name| name.eq_ignore_ascii_case("json"));
    let model = match json_column {
        Some(at) => users_json(reader, at),
        None => users_table(reader, &header),
    };
    match model {
        Err(ParseError::Unrecognised) => Ok(text_fallback("Users", stdout)),
        other => other,
    }
}

/// The objects of the `JSON` column, parsed and written again as one pretty array: the text of the model is made of
/// parsed values and never holds a byte of the output as it came. A cell that is not a JSON object (or is over the
/// cap of a text) is skipped and counted. Collecting stops once the text cap is passed, and the cut says so.
fn users_json(mut reader: Reader<&[u8]>, at: usize) -> Result<ViewModel, ParseError> {
    let mut values: Vec<Value> = Vec::new();
    let mut skipped = 0_usize;
    let mut size = 0_usize;
    let more = read_rows(&mut reader, |record| {
        if size > MAX_TEXT {
            return;
        }
        match record.get(at) {
            Some(cell) if cell.len() <= MAX_TEXT => match serde_json::from_slice::<Value>(cell) {
                Ok(value @ Value::Object(_)) => {
                    size += value.to_string().len();
                    values.push(value);
                }
                _ => skipped += 1,
            },
            _ => skipped += 1,
        }
    })?;
    let shown = values.len();
    let text = serde_json::to_string_pretty(&Value::Array(values)).unwrap_or_else(|_| "[]".to_owned());
    let mut note = format!("{shown} objects shown.");
    if skipped > 0 {
        note.push_str(&format!(" {skipped} cells skipped, they are not JSON objects."));
    }
    if more {
        note.push_str(" More rows exist than are read.");
    }
    Ok(text_view("Users", &text, TextFormat::Json).with_note(&note))
}

// ---------------------------------------------------------------------------------------------------------------------
// The domain.
// ---------------------------------------------------------------------------------------------------------------------

/// The members of `info domain formatjson` the module knows; every other member is dropped.
const DOMAIN_MEMBERS: &[&str] = &["customerDomain", "creationTime", "isVerified", "domainAliases"];

/// A domain as far as the module knows it. It is read from a parsed JSON object only (never from an array, never by
/// position), a member of the wrong type makes the whole document unrecognised, and members that are not named are
/// counted and dropped.
struct Domain {
    name: Option<String>,
    created: Option<String>,
    verified: Option<bool>,
    aliases: Vec<(Option<String>, Option<bool>)>,
    others: usize,
}

/// A text member: `Some(None)` when it is absent or null, `None` when it has another type.
fn text_member(map: &Map<String, Value>, key: &str) -> Option<Option<String>> {
    match map.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(text)) => Some(Some(text.clone())),
        Some(_) => None,
    }
}

/// A flag member, read like [`text_member`].
fn flag_member(map: &Map<String, Value>, key: &str) -> Option<Option<bool>> {
    match map.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(Value::Bool(flag)) => Some(Some(*flag)),
        Some(_) => None,
    }
}

/// The domain of a document, or `None` when it is not the shape the module knows.
fn read_domain(stdout: &[u8]) -> Option<Domain> {
    if stdout.len() > MAX_DOMAIN {
        return None;
    }
    let Value::Object(map) = serde_json::from_slice::<Value>(stdout).ok()? else {
        return None;
    };
    let mut aliases = Vec::new();
    match map.get("domainAliases") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for item in items {
                let Value::Object(alias) = item else {
                    return None;
                };
                aliases.push((text_member(alias, "domainAliasName")?, flag_member(alias, "verified")?));
            }
        }
        Some(_) => return None,
    }
    let domain = Domain {
        name: text_member(&map, "customerDomain")?,
        created: text_member(&map, "creationTime")?,
        verified: flag_member(&map, "isVerified")?,
        aliases,
        others: map.keys().filter(|key| !DOMAIN_MEMBERS.contains(&key.as_str())).count(),
    };
    let known = domain.name.is_some() || domain.created.is_some() || domain.verified.is_some() || !domain.aliases.is_empty();
    known.then_some(domain)
}

fn yes_no(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}

fn domain_view(stdout: &[u8]) -> Result<ViewModel, ParseError> {
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Err(ParseError::Empty);
    }
    let Some(domain) = read_domain(stdout) else {
        return Ok(text_fallback("Domain", stdout));
    };
    let mut rows = KvBuilder::new("Domain");
    if let Some(name) = &domain.name {
        rows.row("Domain", name, Tone::Normal);
    }
    if let Some(created) = &domain.created {
        rows.row("Created", created, Tone::Normal);
    }
    if let Some(verified) = domain.verified {
        rows.row("Verified", yes_no(verified), if verified { Tone::Normal } else { Tone::Warn });
    }
    for (name, verified) in &domain.aliases {
        let name = name.as_deref().unwrap_or("(unnamed)");
        let value = match verified {
            Some(true) => format!("{name}, verified"),
            Some(false) => format!("{name}, not verified"),
            None => name.to_owned(),
        };
        rows.row("Alias", &value, if *verified == Some(false) { Tone::Warn } else { Tone::Normal });
    }
    let model = rows.finish();
    Ok(match domain.others {
        0 => model,
        1 => model.with_note("1 other member is not shown."),
        n => model.with_note(&format!("{n} other members are not shown.")),
    })
}

/// The domain as JSON text, for the person who asks for it: the members the module knows, parsed and written again
/// as one pretty object. Members it does not know are not in it, so nothing else the tool printed can reach the page.
pub fn domain_json_text(stdout: &[u8]) -> Result<ViewModel, ParseError> {
    if stdout.len() > MAX_INPUT {
        return Err(ParseError::TooLarge);
    }
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Err(ParseError::Empty);
    }
    let domain = read_domain(stdout).ok_or(ParseError::Unrecognised)?;
    let mut object = Map::new();
    if let Some(name) = domain.name {
        object.insert("customerDomain".to_owned(), Value::String(name));
    }
    if let Some(created) = domain.created {
        object.insert("creationTime".to_owned(), Value::String(created));
    }
    if let Some(verified) = domain.verified {
        object.insert("isVerified".to_owned(), Value::Bool(verified));
    }
    let aliases: Vec<Value> = domain
        .aliases
        .into_iter()
        .map(|(name, verified)| {
            let mut alias = Map::new();
            if let Some(name) = name {
                alias.insert("domainAliasName".to_owned(), Value::String(name));
            }
            if let Some(verified) = verified {
                alias.insert("verified".to_owned(), Value::Bool(verified));
            }
            Value::Object(alias)
        })
        .collect();
    if !aliases.is_empty() {
        object.insert("domainAliases".to_owned(), Value::Array(aliases));
    }
    let text = serde_json::to_string_pretty(&Value::Object(object)).unwrap_or_else(|_| "{}".to_owned());
    Ok(text_view("Domain", &text, TextFormat::Json))
}
