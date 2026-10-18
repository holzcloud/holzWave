//! What the output of a tool becomes for the page: a table, a key-value list or a text, with caps on size and the
//! cleaning of text that came from a cluster or an account.
//!
//! Event messages, annotations, labels, resource tags, user names and organisation paths are other people's text. A
//! terminal escape in a cell is spoofing, a right-to-left override reorders a name and a 5000-character cell is a
//! freeze. So the cleaning is done once, here, before the page sees a byte: the builders pass every text through
//! [`clean_cell`] (or [`clean_text`] for a text block) and cut it to its cap, and the fields of a model are private, so
//! a model that holds uncleaned or unbounded text cannot be built by the code of a tool module. The page draws a model
//! with `textContent`; no field is markup, a link or a command, whatever it looks like.
//!
//! A model is the second JSON contract of the integrations. `docs/integration-view.schema.json` describes exactly the
//! documents [`ViewModel`] reads and writes, and a parity test holds the two equal. Reading a document checks the
//! shape and the caps; it does not clean text and does not check an action's arguments against a verb, because only a
//! builder knows the verb. The two rules that no JSON Schema keyword can say (a row has as many cells as the table has
//! columns, and row keys are unique) are [`ViewModel::check`].

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::marker::PhantomData;

use serde::de::value::MapAccessDeserializer;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

use super::{Validated, Verb, VerbEffect};
use crate::ai::redact;
use crate::ai::tools::is_hidden;

/// The most columns a table holds; further columns are dropped.
pub const MAX_COLUMNS: usize = 64;
/// The most rows a table (or entries a key-value list) holds; further rows are dropped and the model says so.
pub const MAX_ROWS: usize = 2000;
/// The most bytes in one cell or one value.
pub const MAX_CELL: usize = 4096;
/// The most actions on one row or one view.
pub const MAX_ACTIONS: usize = 8;
/// The most arguments of one action.
pub const MAX_ACTION_ARGS: usize = 8;
/// The most bytes in a row key and in the value of an action argument.
pub const MAX_KEY: usize = 253;
/// The most bytes in a title.
pub const MAX_TITLE: usize = 128;
/// The most bytes in a label, a column id, a verb id, an argument name and a note.
pub const MAX_LABEL: usize = 128;
/// The most bytes in the text of a `text` model; a longer output is cut and the model says so.
pub const MAX_TEXT: usize = 65536;

const ELLIPSIS: char = '\u{2026}';

// ---------------------------------------------------------------------------------------------------------------------
// Cleaning.
// ---------------------------------------------------------------------------------------------------------------------

/// Cleans a one-line text: escape sequences with their payload, control characters and characters with no visible shape
/// are removed, every run of white space (line breaks, tabs, carriage returns, no-break and wide spaces) becomes one
/// plain space and the ends are trimmed. The text is not cut here; see [`clean_cell`].
fn clean_line(text: &str) -> String {
    // A carriage return is white space here: it must not be dropped as a control character, or two words would join.
    let text = text.replace('\r', "\n");
    let cleaned = redact::sanitise(&text).text;
    let mut out = String::with_capacity(cleaned.len());
    let mut gap = false;
    for c in cleaned.chars() {
        if c.is_whitespace() {
            gap = !out.is_empty();
        } else if !is_hidden(c) {
            if gap {
                out.push(' ');
                gap = false;
            }
            out.push(c);
        }
    }
    out
}

/// Cuts `text` to at most `cap` bytes on a character boundary; a cut text ends with an ellipsis that is inside the cap.
/// Says whether it cut.
fn cut_flag(text: String, cap: usize) -> (String, bool) {
    if text.len() <= cap {
        return (text, false);
    }
    let mut end = cap.saturating_sub(ELLIPSIS.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text;
    out.truncate(end);
    out.push(ELLIPSIS);
    (out, true)
}

fn cut(text: String, cap: usize) -> String {
    cut_flag(text, cap).0
}

/// The text of a cell: cleaned of escape sequences, controls and invisible characters, white space collapsed to single
/// spaces, and cut at [`MAX_CELL`] bytes on a character boundary with an ellipsis.
pub fn clean_cell(text: &str) -> String {
    cut(clean_line(text), MAX_CELL)
}

/// The text of a text block: like [`clean_cell`] but line feeds and tabs stay, so an output keeps its lines. Cut at
/// [`MAX_TEXT`] bytes.
pub fn clean_text(text: &str) -> String {
    clean_block(text).0
}

/// [`clean_text`] and whether it had to cut.
fn clean_block(text: &str) -> (String, bool) {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let cleaned = redact::sanitise(&text).text;
    let kept: String = cleaned.chars().filter(|c| matches!(c, '\n' | '\t') || !is_hidden(*c)).collect();
    cut_flag(kept, MAX_TEXT)
}

// ---------------------------------------------------------------------------------------------------------------------
// Bounded values. They are private: a model exposes `&str` and `&[T]`.
// ---------------------------------------------------------------------------------------------------------------------

/// A string of at most `N` bytes. Reading one from JSON refuses a longer text.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
struct Capped<const N: usize>(String);

impl<const N: usize> Capped<N> {
    /// A cleaned one-line text, cut to the cap.
    fn line(text: &str) -> Self {
        Self(cut(clean_line(text), N))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de, const N: usize> Deserialize<'de> for Capped<N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.len() > N {
            return Err(de::Error::custom("a text is longer than its cap"));
        }
        Ok(Self(text))
    }
}

/// A list of at most `N` elements. Reading one from JSON refuses a longer array.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
struct List<T, const N: usize>(Vec<T>);

impl<T, const N: usize> Default for List<T, N> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<'de, T: Deserialize<'de>, const N: usize> Deserialize<'de> for List<T, N> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let items = Vec::<T>::deserialize(deserializer)?;
        if items.len() > N {
            return Err(de::Error::custom("a list is longer than its cap"));
        }
        Ok(Self(items))
    }
}

/// Reads a struct from a JSON object only. A derived `Deserialize` also reads an array of the member values, in order;
/// the schema does not, so every struct of the model is read through this.
fn object_only<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<T, D::Error> {
    struct Members<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for Members<T> {
        type Value = T;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an object")
        }

        fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<T, A::Error> {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(Members(PhantomData))
}

// ---------------------------------------------------------------------------------------------------------------------
// The model.
// ---------------------------------------------------------------------------------------------------------------------

/// How a row is drawn: a second cue besides colour is the page's business.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// Nothing special.
    #[default]
    Normal,
    /// Quieter than the rest: a finished or unimportant entry.
    Dim,
    /// Worth a look: pending, restarting, a warning.
    Warn,
    /// Wrong: failed, crashing, an error.
    Danger,
}

/// How a text is meant to be read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextFormat {
    /// Plain text.
    #[default]
    Plain,
    /// JSON text, which the page may set in a mono block as it is.
    Json,
}

/// One column of a table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Column {
    id: Capped<MAX_LABEL>,
    label: Capped<MAX_LABEL>,
}

impl Column {
    /// The id the rows' cells are read by position of.
    pub fn id(&self) -> &str {
        self.id.as_str()
    }

    /// The heading the person reads.
    pub fn label(&self) -> &str {
        self.label.as_str()
    }
}

impl<'de> Deserialize<'de> for Column {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            id: Capped<MAX_LABEL>,
            label: Capped<MAX_LABEL>,
        }
        let Raw { id, label } = object_only(deserializer)?;
        Ok(Self { id, label })
    }
}

/// A button on a row or a view: a verb of a tool with the arguments it was validated with. It exists only through
/// [`RowBuilder::action`] and [`KvBuilder::action`], which build it from a verb of a spec and accept it only when
/// [`Validated::parse`] accepts the arguments. Its effect is the verb's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RowAction {
    verb: Capped<MAX_LABEL>,
    label: Capped<MAX_LABEL>,
    effect: VerbEffect,
    args: BTreeMap<Capped<MAX_LABEL>, Capped<MAX_KEY>>,
}

impl RowAction {
    /// The id of the verb, which the page sends back with the arguments.
    pub fn verb(&self) -> &str {
        self.verb.as_str()
    }

    /// The label on the button.
    pub fn label(&self) -> &str {
        self.label.as_str()
    }

    /// What the verb does to the world; the page shows a card for everything but a read.
    pub fn effect(&self) -> VerbEffect {
        self.effect
    }

    /// The arguments as (name, value). The core validates them again when the page sends them back.
    pub fn args(&self) -> impl Iterator<Item = (&str, &str)> {
        self.args.iter().map(|(name, value)| (name.as_str(), value.as_str()))
    }
}

impl<'de> Deserialize<'de> for RowAction {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            verb: Capped<MAX_LABEL>,
            label: Capped<MAX_LABEL>,
            effect: VerbEffect,
            args: BTreeMap<Capped<MAX_LABEL>, Capped<MAX_KEY>>,
        }
        let Raw { verb, label, effect, args } = object_only(deserializer)?;
        if args.len() > MAX_ACTION_ARGS {
            return Err(de::Error::custom("an action holds more arguments than its cap"));
        }
        Ok(Self { verb, label, effect, args })
    }
}

type Actions = List<RowAction, MAX_ACTIONS>;

/// One row of a table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    key: Capped<MAX_KEY>,
    cells: List<Capped<MAX_CELL>, MAX_COLUMNS>,
    tone: Tone,
    actions: Actions,
}

impl Row {
    /// The identity of the row among the rows of its table: unique, and stable between two reads of the same list.
    pub fn key(&self) -> &str {
        self.key.as_str()
    }

    /// The cells, one per column.
    pub fn cells(&self) -> impl ExactSizeIterator<Item = &str> {
        self.cells.0.iter().map(Capped::as_str)
    }

    /// How the row is drawn.
    pub fn tone(&self) -> Tone {
        self.tone
    }

    /// The row's actions.
    pub fn actions(&self) -> &[RowAction] {
        &self.actions.0
    }
}

impl<'de> Deserialize<'de> for Row {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            key: Capped<MAX_KEY>,
            cells: List<Capped<MAX_CELL>, MAX_COLUMNS>,
            #[serde(default)]
            tone: Tone,
            #[serde(default)]
            actions: Actions,
        }
        let Raw { key, cells, tone, actions } = object_only(deserializer)?;
        Ok(Self { key, cells, tone, actions })
    }
}

/// The note that says a table shows only the first rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Truncation {
    shown: u32,
    note: Capped<MAX_LABEL>,
}

impl Truncation {
    /// How many rows the table holds.
    pub fn shown(&self) -> u32 {
        self.shown
    }

    /// What the person reads.
    pub fn note(&self) -> &str {
        self.note.as_str()
    }
}

impl<'de> Deserialize<'de> for Truncation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            shown: u32,
            note: Capped<MAX_LABEL>,
        }
        let Raw { shown, note } = object_only(deserializer)?;
        Ok(Self { shown, note })
    }
}

/// One entry of a key-value list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KvRow {
    label: Capped<MAX_LABEL>,
    value: Capped<MAX_CELL>,
    tone: Tone,
}

impl KvRow {
    /// The name of the entry.
    pub fn label(&self) -> &str {
        self.label.as_str()
    }

    /// Its value.
    pub fn value(&self) -> &str {
        self.value.as_str()
    }

    /// How the entry is drawn.
    pub fn tone(&self) -> Tone {
        self.tone
    }
}

impl<'de> Deserialize<'de> for KvRow {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            label: Capped<MAX_LABEL>,
            value: Capped<MAX_CELL>,
            #[serde(default)]
            tone: Tone,
        }
        let Raw { label, value, tone } = object_only(deserializer)?;
        Ok(Self { label, value, tone })
    }
}

/// A table: columns, rows with a key, cells, a tone and row actions, and an optional note that the rows were cut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TableView {
    title: Capped<MAX_TITLE>,
    columns: List<Column, MAX_COLUMNS>,
    rows: List<Row, MAX_ROWS>,
    truncated: Option<Truncation>,
    note: Option<Capped<MAX_LABEL>>,
    fetched: Option<u32>,
}

impl TableView {
    /// The title of the block.
    pub fn title(&self) -> &str {
        self.title.as_str()
    }

    /// The columns.
    pub fn columns(&self) -> &[Column] {
        &self.columns.0
    }

    /// The rows.
    pub fn rows(&self) -> &[Row] {
        &self.rows.0
    }

    /// Set when rows were dropped.
    pub fn truncated(&self) -> Option<&Truncation> {
        self.truncated.as_ref()
    }

    /// A line of context under the table.
    pub fn note(&self) -> Option<&str> {
        self.note.as_ref().map(Capped::as_str)
    }

    /// When the data was read, in seconds since the Unix epoch.
    pub fn fetched(&self) -> Option<u32> {
        self.fetched
    }
}

impl<'de> Deserialize<'de> for TableView {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            title: Capped<MAX_TITLE>,
            columns: List<Column, MAX_COLUMNS>,
            rows: List<Row, MAX_ROWS>,
            truncated: Option<Truncation>,
            note: Option<Capped<MAX_LABEL>>,
            fetched: Option<u32>,
        }
        let Raw { title, columns, rows, truncated, note, fetched } = object_only(deserializer)?;
        Ok(Self { title, columns, rows, truncated, note, fetched })
    }
}

/// A list of labelled values with actions of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KvView {
    title: Capped<MAX_TITLE>,
    rows: List<KvRow, MAX_ROWS>,
    actions: Actions,
    note: Option<Capped<MAX_LABEL>>,
    fetched: Option<u32>,
}

impl KvView {
    /// The title of the block.
    pub fn title(&self) -> &str {
        self.title.as_str()
    }

    /// The entries.
    pub fn rows(&self) -> &[KvRow] {
        &self.rows.0
    }

    /// The actions of the whole view.
    pub fn actions(&self) -> &[RowAction] {
        &self.actions.0
    }

    /// A line of context under the list.
    pub fn note(&self) -> Option<&str> {
        self.note.as_ref().map(Capped::as_str)
    }

    /// When the data was read, in seconds since the Unix epoch.
    pub fn fetched(&self) -> Option<u32> {
        self.fetched
    }
}

impl<'de> Deserialize<'de> for KvView {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            title: Capped<MAX_TITLE>,
            rows: List<KvRow, MAX_ROWS>,
            #[serde(default)]
            actions: Actions,
            note: Option<Capped<MAX_LABEL>>,
            fetched: Option<u32>,
        }
        let Raw { title, rows, actions, note, fetched } = object_only(deserializer)?;
        Ok(Self { title, rows, actions, note, fetched })
    }
}

/// A text, plain or JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextView {
    title: Capped<MAX_TITLE>,
    text: Capped<MAX_TEXT>,
    format: TextFormat,
    truncated: bool,
    note: Option<Capped<MAX_LABEL>>,
    fetched: Option<u32>,
}

impl TextView {
    /// The title of the block.
    pub fn title(&self) -> &str {
        self.title.as_str()
    }

    /// The text.
    pub fn text(&self) -> &str {
        self.text.as_str()
    }

    /// How the text is meant to be read.
    pub fn format(&self) -> TextFormat {
        self.format
    }

    /// Whether the text is only the start of what the tool wrote.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// A line of context under the text.
    pub fn note(&self) -> Option<&str> {
        self.note.as_ref().map(Capped::as_str)
    }

    /// When the data was read, in seconds since the Unix epoch.
    pub fn fetched(&self) -> Option<u32> {
        self.fetched
    }
}

impl<'de> Deserialize<'de> for TextView {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            title: Capped<MAX_TITLE>,
            text: Capped<MAX_TEXT>,
            #[serde(default)]
            format: TextFormat,
            #[serde(default)]
            truncated: bool,
            note: Option<Capped<MAX_LABEL>>,
            fetched: Option<u32>,
        }
        let Raw { title, text, format, truncated, note, fetched } = object_only(deserializer)?;
        Ok(Self { title, text, format, truncated, note, fetched })
    }
}

/// The one thing the page ever sees of a tool's output. The JSON has a `kind` member: `table`, `kv` or `text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewModel {
    /// Columns and rows.
    Table(TableView),
    /// Labelled values.
    Kv(KvView),
    /// A text.
    Text(TextView),
}

impl<'de> Deserialize<'de> for ViewModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The tag is read by the derive, and an internally tagged enum also reads an array that starts with the tag, so
        // the document is first required to be an object.
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Tagged {
            Table(TableView),
            Kv(KvView),
            Text(TextView),
        }
        Ok(match object_only(deserializer)? {
            Tagged::Table(view) => Self::Table(view),
            Tagged::Kv(view) => Self::Kv(view),
            Tagged::Text(view) => Self::Text(view),
        })
    }
}

/// Why a model that was read from JSON breaks a rule that the schema cannot state. Fixed sentences: no text of the
/// model is in an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewError {
    /// A row has a different number of cells than the table has columns.
    CellCount,
    /// Two rows of a table share a key.
    DuplicateKey,
}

impl ViewError {
    /// The fixed sentence of the refusal.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CellCount => "a row has a different number of cells than the table has columns",
            Self::DuplicateKey => "two rows of a table share a key",
        }
    }
}

impl fmt::Display for ViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for ViewError {}

impl ViewModel {
    /// The title of the block, whatever its kind.
    pub fn title(&self) -> &str {
        match self {
            Self::Table(view) => view.title(),
            Self::Kv(view) => view.title(),
            Self::Text(view) => view.title(),
        }
    }

    /// The same model with the time it was read, in seconds since the Unix epoch.
    #[must_use]
    pub fn with_fetched(mut self, seconds: u32) -> Self {
        match &mut self {
            Self::Table(view) => view.fetched = Some(seconds),
            Self::Kv(view) => view.fetched = Some(seconds),
            Self::Text(view) => view.fetched = Some(seconds),
        }
        self
    }

    /// The same model with a line of context, cleaned and cut like a label.
    #[must_use]
    pub fn with_note(mut self, note: &str) -> Self {
        let note = Some(Capped::line(note));
        match &mut self {
            Self::Table(view) => view.note = note,
            Self::Kv(view) => view.note = note,
            Self::Text(view) => view.note = note,
        }
        self
    }

    /// The two rules of the contract that no JSON Schema keyword can state, which reading a document does not check:
    /// every row of a table has as many cells as the table has columns, and the keys of the rows are unique. A model
    /// from the builders always passes.
    pub fn check(&self) -> Result<(), ViewError> {
        let Self::Table(table) = self else {
            return Ok(());
        };
        let columns = table.columns.0.len();
        if table.rows.0.iter().any(|row| row.cells.0.len() != columns) {
            return Err(ViewError::CellCount);
        }
        let mut seen = HashSet::with_capacity(table.rows.0.len());
        if table.rows.0.iter().any(|row| !seen.insert(row.key.as_str())) {
            return Err(ViewError::DuplicateKey);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Builders.
// ---------------------------------------------------------------------------------------------------------------------

/// The actions of a row or a view: built from a verb, accepted only when its arguments validate.
fn push_action(actions: &mut Vec<RowAction>, verb: &'static Verb, args: &[(&str, &str)]) -> bool {
    if actions.len() >= MAX_ACTIONS || args.len() > MAX_ACTION_ARGS {
        return false;
    }
    let mut pairs = BTreeMap::new();
    for (name, value) in args {
        // A value that is too long is refused and never cut: a cut name is another name.
        if value.len() > MAX_KEY || pairs.insert((*name).to_owned(), (*value).to_owned()).is_some() {
            return false;
        }
    }
    if Validated::parse(verb, &pairs).is_err() {
        return false;
    }
    let args = pairs.into_iter().map(|(name, value)| (Capped(name), Capped(value))).collect();
    actions.push(RowAction { verb: Capped::line(verb.id), label: Capped::line(verb.label), effect: verb.effect, args });
    true
}

/// One row on its way into a [`TableBuilder`].
#[derive(Debug, Clone)]
pub struct RowBuilder {
    key: String,
    cells: Vec<String>,
    tone: Tone,
    actions: Vec<RowAction>,
}

impl RowBuilder {
    /// A row with a key and its cells. Every text is cleaned; the key is cut to [`MAX_KEY`] bytes and each cell to
    /// [`MAX_CELL`].
    pub fn new(key: &str, cells: &[&str]) -> Self {
        Self {
            key: Capped::<MAX_KEY>::line(key).0,
            cells: cells.iter().take(MAX_COLUMNS).map(|cell| clean_cell(cell)).collect(),
            tone: Tone::Normal,
            actions: Vec::new(),
        }
    }

    /// Sets how the row is drawn.
    pub fn tone(&mut self, tone: Tone) -> &mut Self {
        self.tone = tone;
        self
    }

    /// Adds the action of `verb` with `args` (argument name, value) when [`Validated::parse`] accepts them; says
    /// whether it did. A refused action is not added, so a row whose name fails validation is shown without its
    /// buttons. The label and the effect are the verb's and cannot be set by the caller. A ninth action is not added.
    pub fn action(&mut self, verb: &'static Verb, args: &[(&str, &str)]) -> bool {
        push_action(&mut self.actions, verb, args)
    }
}

/// Builds a table within the caps: 64 columns, 2000 rows, rows of exactly as many cells as columns, unique keys.
#[derive(Debug, Clone)]
pub struct TableBuilder {
    title: Capped<MAX_TITLE>,
    columns: Vec<Column>,
    dropped_columns: usize,
    rows: Vec<Row>,
    keys: HashSet<String>,
    dropped_rows: usize,
    pending: Vec<RowBuilder>,
}

impl TableBuilder {
    /// A table with a title.
    pub fn new(title: &str) -> Self {
        Self {
            title: Capped::line(title),
            columns: Vec::new(),
            dropped_columns: 0,
            rows: Vec::new(),
            keys: HashSet::new(),
            dropped_rows: 0,
            pending: Vec::new(),
        }
    }

    /// Adds a column; the 65th and later are dropped.
    pub fn column(&mut self, id: &str, label: &str) -> &mut Self {
        if self.columns.len() >= MAX_COLUMNS {
            self.dropped_columns += 1;
        } else {
            self.columns.push(Column { id: Capped::line(id), label: Capped::line(label) });
        }
        self
    }

    /// Adds a row; the 2001st and later are dropped and counted. A row whose key is already taken gets `#2`, `#3` and so
    /// on after it, so the keys stay unique.
    pub fn push(&mut self, row: RowBuilder) -> &mut Self {
        if self.rows.len() + self.pending.len() >= MAX_ROWS {
            self.dropped_rows += 1;
        } else {
            self.pending.push(row);
        }
        self
    }

    fn unique_key(&mut self, key: &str) -> String {
        if self.keys.insert(key.to_owned()) {
            return key.to_owned();
        }
        for n in 2_usize.. {
            let suffix = format!("#{n}");
            let mut base = key.to_owned();
            let mut end = MAX_KEY - suffix.len();
            while !base.is_char_boundary(end.min(base.len())) {
                end -= 1;
            }
            base.truncate(end.min(base.len()));
            base.push_str(&suffix);
            if self.keys.insert(base.clone()) {
                return base;
            }
        }
        unreachable!("an unbounded counter finds a free key")
    }

    /// The model. Rows are given exactly as many cells as there are columns (missing cells are empty, extra cells are
    /// dropped); a cut table says so in `truncated` and, for columns, in `note`.
    pub fn finish(mut self) -> ViewModel {
        let width = self.columns.len();
        for pending in std::mem::take(&mut self.pending) {
            let key = self.unique_key(&pending.key);
            let mut cells: Vec<Capped<MAX_CELL>> = pending.cells.into_iter().map(Capped).collect();
            cells.resize(width, Capped(String::new()));
            self.rows.push(Row { key: Capped(key), cells: List(cells), tone: pending.tone, actions: List(pending.actions) });
        }
        let shown = self.rows.len();
        let truncated = (self.dropped_rows > 0).then(|| Truncation {
            shown: u32::try_from(shown).unwrap_or(u32::MAX),
            note: Capped::line(&format!("Showing the first {shown} of {} rows.", shown + self.dropped_rows)),
        });
        let note = (self.dropped_columns > 0).then(|| {
            Capped::line(&format!("Showing the first {width} of {} columns.", width + self.dropped_columns))
        });
        ViewModel::Table(TableView {
            title: self.title,
            columns: List(self.columns),
            rows: List(self.rows),
            truncated,
            note,
            fetched: None,
        })
    }
}

/// Builds a key-value list within the caps.
#[derive(Debug, Clone)]
pub struct KvBuilder {
    title: Capped<MAX_TITLE>,
    rows: Vec<KvRow>,
    dropped: usize,
    actions: Vec<RowAction>,
}

impl KvBuilder {
    /// A list with a title.
    pub fn new(title: &str) -> Self {
        Self { title: Capped::line(title), rows: Vec::new(), dropped: 0, actions: Vec::new() }
    }

    /// Adds an entry; the 2001st and later are dropped and counted.
    pub fn row(&mut self, label: &str, value: &str, tone: Tone) -> &mut Self {
        if self.rows.len() >= MAX_ROWS {
            self.dropped += 1;
        } else {
            self.rows.push(KvRow { label: Capped::line(label), value: Capped(clean_cell(value)), tone });
        }
        self
    }

    /// Adds an action of the whole view; see [`RowBuilder::action`].
    pub fn action(&mut self, verb: &'static Verb, args: &[(&str, &str)]) -> bool {
        push_action(&mut self.actions, verb, args)
    }

    /// The model; entries that were dropped are named in `note`.
    pub fn finish(self) -> ViewModel {
        let shown = self.rows.len();
        let note = (self.dropped > 0)
            .then(|| Capped::line(&format!("Showing the first {shown} of {} entries.", shown + self.dropped)));
        ViewModel::Kv(KvView {
            title: self.title,
            rows: List(self.rows),
            actions: List(self.actions),
            note,
            fetched: None,
        })
    }
}

/// A text model of `text`, cleaned (line breaks stay) and cut at [`MAX_TEXT`] bytes with `truncated` set when it was cut.
pub fn text_view(title: &str, text: &str, format: TextFormat) -> ViewModel {
    let (cleaned, cut) = clean_block(text);
    ViewModel::Text(TextView {
        title: Capped::line(title),
        text: Capped(cleaned),
        format,
        truncated: cut,
        note: None,
        fetched: None,
    })
}

/// The model for output that a parser could not read: a plain text of the cleaned, bounded output, with a note that the
/// shape was not recognised. Invalid UTF-8 is replaced. Drift in a tool's output degrades to readable text and is never
/// an error. Only the first [`MAX_TEXT`] bytes are looked at; a longer output sets `truncated`.
pub fn text_fallback(title: &str, output: &[u8]) -> ViewModel {
    let seen = &output[..output.len().min(MAX_TEXT)];
    let (cleaned, cut) = clean_block(&String::from_utf8_lossy(seen));
    let cut_by_cap = output.len() > MAX_TEXT || cut;
    ViewModel::Text(TextView {
        title: Capped::line(title),
        text: Capped(cleaned),
        format: TextFormat::Plain,
        truncated: cut_by_cap,
        note: Some(Capped::line(if cut_by_cap {
            "The output was not in the expected shape and is shown as text, cut at 64 KiB."
        } else {
            "The output was not in the expected shape and is shown as text."
        })),
        fetched: None,
    })
}
