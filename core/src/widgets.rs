//! The widgets of the sidebar: small panels described by data, never by code.
//!
//! There are three kinds. A `note` is a text the person writes; a `timer` is a work and break clock (Pomodoro); a `list` is
//! a list of labelled commands and links: a command is put on the prompt (never run), a link is opened by the system.
//! Two exist from the start (`notes`, `pomodoro`); a person adds more as `widgets/<id>.json` in the app folder. Every file
//! is validated field by field and one that is not valid is left out with a note, so a hand edit can break its own widget
//! and nothing else. Nothing in a widget runs: no script, no HTML, no URL scheme but http and https.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::persist::{read_limited, write_atomic};
use crate::{Error, Result};

pub const DIR: &str = "widgets";
pub const NOTES_FILE: &str = "widget_notes.json";
/// The most widgets shown, and the most items in a list.
pub const MAX_WIDGETS: usize = 24;
pub const MAX_ITEMS: usize = 50;
/// The largest widget file and the largest note.
pub const MAX_FILE: u64 = 64 * 1024;
pub const MAX_NOTE: usize = 64 * 1024;
const MAX_COMMAND: usize = 1024;
const MAX_URL: usize = 2048;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Item {
    pub label: String,
    /// Put on the prompt of the focused pane, never run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Opened with the system's handler.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Body {
    Note,
    Timer { work_min: u32, break_min: u32 },
    List { items: Vec<Item> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WidgetDef {
    pub id: String,
    pub title: String,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawItem {
    label: String,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    id: Option<String>,
    title: String,
    kind: String,
    #[serde(default)]
    work_min: Option<u32>,
    #[serde(default)]
    break_min: Option<u32>,
    #[serde(default)]
    items: Vec<RawItem>,
}

fn plain(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max && !text.chars().any(char::is_control)
}

/// A widget id: lowercase letters, digits, `-` and `_`.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

fn valid_url(url: &str) -> bool {
    url.len() <= MAX_URL
        && (url.starts_with("https://") || url.starts_with("http://"))
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && url.split_once("://").is_some_and(|(_, rest)| !rest.is_empty() && !rest.starts_with('/'))
}

/// Check one widget file's text; `stem` is the file name without `.json`.
pub fn parse(text: &str, stem: &str) -> std::result::Result<WidgetDef, String> {
    let raw: Raw = serde_json::from_str(text).map_err(|e| format!("not a valid widget: {e}"))?;
    if raw.version.is_some_and(|v| v != 1) {
        return Err("a widget of a newer version".into());
    }
    let id = raw.id.unwrap_or_else(|| stem.to_owned());
    if !valid_id(&id) {
        return Err(format!("the id {id:?} is not lowercase letters, digits, - and _ (at most 32)"));
    }
    if !plain(&raw.title, 40) {
        return Err("the title is empty, over 40 characters or has a control character".into());
    }
    let body = match raw.kind.as_str() {
        "note" => Body::Note,
        "timer" => {
            let (work, rest) = (raw.work_min.unwrap_or(25), raw.break_min.unwrap_or(5));
            if !(1..=180).contains(&work) || rest > 60 {
                return Err("a timer works 1 to 180 minutes and rests 0 to 60".into());
            }
            Body::Timer { work_min: work, break_min: rest }
        }
        "list" => {
            if raw.items.is_empty() || raw.items.len() > MAX_ITEMS {
                return Err(format!("a list has 1 to {MAX_ITEMS} items"));
            }
            let mut items = Vec::new();
            for (n, item) in raw.items.into_iter().enumerate() {
                let at = n + 1;
                if !plain(&item.label, 60) {
                    return Err(format!("item {at}: the label is empty, over 60 characters or has a control character"));
                }
                match (&item.command, &item.url) {
                    (Some(c), None) if plain(c, MAX_COMMAND) => {}
                    (None, Some(u)) if valid_url(u) => {}
                    (Some(_), None) => return Err(format!("item {at}: the command is empty, too long or has a control character (one line only)")),
                    (None, Some(_)) => return Err(format!("item {at}: the url must be http or https, without spaces")),
                    _ => return Err(format!("item {at}: give either a command or a url")),
                }
                items.push(Item { label: item.label, command: item.command, url: item.url });
            }
            Body::List { items }
        }
        other => return Err(format!("the kind {other:?} is not note, timer or list")),
    };
    Ok(WidgetDef { id, title: raw.title, body })
}

/// The two widgets every installation has.
pub fn builtin() -> Vec<WidgetDef> {
    vec![
        WidgetDef { id: "notes".into(), title: "Notes".into(), body: Body::Note },
        WidgetDef { id: "pomodoro".into(), title: "Pomodoro".into(), body: Body::Timer { work_min: 25, break_min: 5 } },
    ]
}

/// The built-in widgets and those of `<dir>/widgets/*.json` (a file with the id of a built-in replaces it), and a note for
/// every file that could not be used. Never fails.
pub fn load(dir: &Path) -> (Vec<WidgetDef>, Vec<String>) {
    let mut defs = builtin();
    let mut notes = Vec::new();
    let Ok(read) = fs::read_dir(dir.join(DIR)) else { return (defs, notes) };
    let mut files: Vec<_> = read.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "json")).collect();
    files.sort_by_key(fs::DirEntry::file_name);
    for entry in files {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stem = name.trim_end_matches(".json");
        let text = match read_limited(&entry.path(), MAX_FILE) {
            Ok(Some(b)) => String::from_utf8(b).map_err(|_| "not UTF-8 text".to_owned()),
            Ok(None) => continue,
            Err(e) => Err(e.to_string()),
        };
        match text.and_then(|t| parse(&t, stem)) {
            Ok(def) => {
                if let Some(old) = defs.iter_mut().find(|d| d.id == def.id) {
                    *old = def;
                } else if defs.len() < MAX_WIDGETS {
                    defs.push(def);
                } else {
                    notes.push(format!("{DIR}/{name}: more than {MAX_WIDGETS} widgets, this one is left out"));
                }
            }
            Err(why) => notes.push(format!("{DIR}/{name}: {why}")),
        }
    }
    (defs, notes)
}

/// The saved texts of the note widgets, by widget id. A damaged file is no notes.
pub fn load_notes(dir: &Path) -> BTreeMap<String, String> {
    let bytes = match read_limited(&dir.join(NOTES_FILE), (MAX_NOTE * MAX_WIDGETS) as u64) {
        Ok(Some(b)) => b,
        _ => return BTreeMap::new(),
    };
    let mut map: BTreeMap<String, String> = serde_json::from_slice(&bytes).unwrap_or_default();
    map.retain(|id, text| valid_id(id) && text.len() <= MAX_NOTE);
    map
}

/// Save the text of the note widget `id`. An empty text removes it.
pub fn save_note(dir: &Path, id: &str, text: &str) -> Result<()> {
    if !valid_id(id) {
        return Err(Error::Unsupported("not a widget id".into()));
    }
    if text.len() > MAX_NOTE {
        return Err(Error::Unsupported(format!("a note is at most {} KiB", MAX_NOTE / 1024)));
    }
    let mut map = load_notes(dir);
    if text.is_empty() {
        map.remove(id);
    } else if map.len() >= MAX_WIDGETS && !map.contains_key(id) {
        return Err(Error::Unsupported("too many notes".into()));
    } else {
        map.insert(id.to_owned(), text.to_owned());
    }
    Ok(write_atomic(&dir.join(NOTES_FILE), &serde_json::to_vec_pretty(&map).unwrap_or_default())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("hw-widgets-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join(DIR)).unwrap();
        d
    }

    #[test]
    fn the_two_builtins_are_always_there() {
        let (defs, notes) = load(&dir("a"));
        assert_eq!(defs.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), vec!["notes", "pomodoro"]);
        assert!(notes.is_empty());
        assert_eq!(load(Path::new("/does/not/exist")).0.len(), 2);
    }

    #[test]
    fn a_list_of_commands_and_links_is_read_and_a_file_can_replace_a_builtin() {
        let d = dir("b");
        fs::write(
            d.join(DIR).join("deploy.json"),
            r#"{"title":"Deploy","kind":"list","items":[{"label":"Pods","command":"kubectl get pods"},{"label":"Docs","url":"https://example.org/x?a=1"}]}"#,
        )
        .unwrap();
        fs::write(d.join(DIR).join("pomodoro.json"), r#"{"title":"Focus","kind":"timer","work_min":50,"break_min":10}"#).unwrap();
        let (defs, notes) = load(&d);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(defs.len(), 3);
        assert_eq!(defs[1].body, Body::Timer { work_min: 50, break_min: 10 });
        let Body::List { items } = &defs[2].body else { panic!() };
        assert_eq!(items[0].command.as_deref(), Some("kubectl get pods"));
        assert_eq!(items[1].url.as_deref(), Some("https://example.org/x?a=1"));
        let json = serde_json::to_value(&defs[2]).unwrap();
        assert_eq!(json["kind"], "list");
        assert!(json["items"][0].get("url").is_none());
    }

    #[test]
    fn what_could_run_or_escape_is_refused_with_a_reason() {
        let bad = |text: &str| parse(text, "x").unwrap_err();
        assert!(bad(r#"{"title":"T","kind":"list","items":[{"label":"a","url":"javascript:alert(1)"}]}"#).contains("http or https"));
        assert!(bad(r#"{"title":"T","kind":"list","items":[{"label":"a","url":"file:///etc/passwd"}]}"#).contains("http or https"));
        assert!(bad(r#"{"title":"T","kind":"list","items":[{"label":"a","url":"https://a b"}]}"#).contains("without spaces"));
        assert!(bad(r#"{"title":"T","kind":"list","items":[{"label":"a","command":"ls\nrm -rf /"}]}"#).contains("one line"));
        assert!(bad(r#"{"title":"T","kind":"list","items":[{"label":"a","command":"ls","url":"https://x.y"}]}"#).contains("either"));
        assert!(bad(r#"{"title":"T","kind":"list","items":[]}"#).contains("1 to"));
        assert!(bad(r#"{"title":"T","kind":"script","code":"x"}"#).contains("unknown field") || bad(r#"{"title":"T","kind":"script"}"#).contains("not note"));
        assert!(bad(r#"{"title":"T","kind":"note","html":"<b>"}"#).contains("unknown field"));
        assert!(bad(r#"{"title":"","kind":"note"}"#).contains("title"));
        assert!(bad(r#"{"title":"T","kind":"note","id":"../x"}"#).contains("id"));
        assert!(bad(r#"{"title":"T","kind":"timer","work_min":0}"#).contains("timer"));
        assert!(bad(r#"{"version":2,"title":"T","kind":"note"}"#).contains("newer"));
        assert!(bad("not json").contains("not a valid widget"));
    }

    #[test]
    fn a_widget_and_its_items_are_json_objects_never_positional_arrays() {
        // serde would read a struct from an array of its member values; nothing documents that and no schema describes it.
        let bad = |text: &str| parse(text, "x").unwrap_err();
        assert!(bad(r#"[null,null,"T","note"]"#).contains("a widget file is a JSON object with named members"));
        assert!(bad(r#"[]"#).contains("a widget file is a JSON object with named members"));
        assert!(bad(r#"{"title":"T","kind":"list","items":[["Pods","kubectl get pods",null]]}"#)
            .contains("an item is a JSON object with named members"));
        assert!(bad(r#"{"title":"T","kind":"note","items":[["Pods"]]}"#).contains("an item is a JSON object with named members"));
        assert!(parse(r#"{"title":"T","kind":"list","items":[{"label":"a","command":"ls"}]}"#, "x").is_ok());
    }

    #[test]
    fn one_bad_file_costs_one_widget_and_says_which() {
        let d = dir("c");
        fs::write(d.join(DIR).join("good.json"), r#"{"title":"Good","kind":"note"}"#).unwrap();
        fs::write(d.join(DIR).join("bad.json"), r#"{"title":"Bad","kind":"nope"}"#).unwrap();
        fs::write(d.join(DIR).join("big.json"), "x".repeat(MAX_FILE as usize + 1)).unwrap();
        let (defs, notes) = load(&d);
        assert!(defs.iter().any(|w| w.id == "good"));
        assert_eq!(notes.len(), 2);
        assert!(notes.iter().any(|n| n.starts_with("widgets/bad.json")));
    }

    #[test]
    fn notes_are_saved_bounded_and_forgotten_when_emptied() {
        let d = dir("d");
        save_note(&d, "notes", "buy wood").unwrap();
        save_note(&d, "other", "x").unwrap();
        assert_eq!(load_notes(&d).get("notes").map(String::as_str), Some("buy wood"));
        assert!(save_note(&d, "../x", "x").is_err());
        assert!(save_note(&d, "notes", &"x".repeat(MAX_NOTE + 1)).is_err());
        save_note(&d, "notes", "").unwrap();
        assert!(!load_notes(&d).contains_key("notes"));
        assert!(load_notes(&d).contains_key("other"));
        fs::write(d.join(NOTES_FILE), "garbage").unwrap();
        assert!(load_notes(&d).is_empty());
    }
}
