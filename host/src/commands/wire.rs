//! The wire types of the arguments and answers of the commands: what the page sends and reads, as serde names it. The
//! page tests and the protocol golden pin the spelling, so a name here never changes without both.
//!
//! The op types are internally tagged with `kind` and every variant, motion and mode name is snake_case, so the page
//! sends `{kind: "begin", x, y, mode: "word"}` and `{kind: "move", motion: "line_start"}`.

use holzwave_core::blocks::Grant;
use holzwave_core::engine::{
    CopyMotion, CopyOp, CopyState, MouseAction, MouseButton, PasteOutcome, SearchOp, SelectMode, SelectOp,
};
use holzwave_core::pane::EditAction;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectModeMsg {
    Cell,
    Word,
    Line,
}

impl From<SelectModeMsg> for SelectMode {
    fn from(mode: SelectModeMsg) -> Self {
        match mode {
            SelectModeMsg::Cell => SelectMode::Cell,
            SelectModeMsg::Word => SelectMode::Word,
            SelectModeMsg::Line => SelectMode::Line,
        }
    }
}

/// The line-editing steps the page can ask for by name: `"word_back"`, `"word_forward"`, `"line_start"`, `"line_end"`,
/// `"delete_word_back"`, `"delete_to_line_start"`. The core owns the bytes; an unknown name is refused by serde.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditActionMsg {
    WordBack,
    WordForward,
    LineStart,
    LineEnd,
    DeleteWordBack,
    DeleteToLineStart,
}

impl From<EditActionMsg> for EditAction {
    fn from(action: EditActionMsg) -> Self {
        match action {
            EditActionMsg::WordBack => EditAction::WordBack,
            EditActionMsg::WordForward => EditAction::WordForward,
            EditActionMsg::LineStart => EditAction::LineStart,
            EditActionMsg::LineEnd => EditAction::LineEnd,
            EditActionMsg::DeleteWordBack => EditAction::DeleteWordBack,
            EditActionMsg::DeleteToLineStart => EditAction::DeleteToLineStart,
        }
    }
}

/// `{kind: "begin", x, y, mode}`, `{kind: "extend", x, y}`, `{kind: "all"}`, `{kind: "clear"}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelectOpMsg {
    Begin { x: u16, y: u16, mode: SelectModeMsg },
    Extend { x: u16, y: u16 },
    All,
    Clear,
}

impl From<SelectOpMsg> for SelectOp {
    fn from(op: SelectOpMsg) -> Self {
        match op {
            SelectOpMsg::Begin { x, y, mode } => SelectOp::Begin { x, y, mode: mode.into() },
            SelectOpMsg::Extend { x, y } => SelectOp::Extend { x, y },
            SelectOpMsg::All => SelectOp::All,
            SelectOpMsg::Clear => SelectOp::Clear,
        }
    }
}

/// `{kind: "set", needle}`, `{kind: "next"}`, `{kind: "prev"}`, `{kind: "clear"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchOpMsg {
    Set { needle: String },
    Next,
    Prev,
    Clear,
}

impl From<SearchOpMsg> for SearchOp {
    fn from(op: SearchOpMsg) -> Self {
        match op {
            SearchOpMsg::Set { needle } => SearchOp::Set(needle),
            SearchOpMsg::Next => SearchOp::Next,
            SearchOpMsg::Prev => SearchOp::Prev,
            SearchOpMsg::Clear => SearchOp::Clear,
        }
    }
}

/// `left|right|up|down|line_start|line_end|top|bottom|page_up|page_down|word_forward|word_back`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyMotionMsg {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
    Top,
    Bottom,
    PageUp,
    PageDown,
    WordForward,
    WordBack,
}

impl From<CopyMotionMsg> for CopyMotion {
    fn from(motion: CopyMotionMsg) -> Self {
        match motion {
            CopyMotionMsg::Left => CopyMotion::Left,
            CopyMotionMsg::Right => CopyMotion::Right,
            CopyMotionMsg::Up => CopyMotion::Up,
            CopyMotionMsg::Down => CopyMotion::Down,
            CopyMotionMsg::LineStart => CopyMotion::LineStart,
            CopyMotionMsg::LineEnd => CopyMotion::LineEnd,
            CopyMotionMsg::Top => CopyMotion::Top,
            CopyMotionMsg::Bottom => CopyMotion::Bottom,
            CopyMotionMsg::PageUp => CopyMotion::PageUp,
            CopyMotionMsg::PageDown => CopyMotion::PageDown,
            CopyMotionMsg::WordForward => CopyMotion::WordForward,
            CopyMotionMsg::WordBack => CopyMotion::WordBack,
        }
    }
}

/// `{kind: "enter"}`, `{kind: "exit"}`, `{kind: "toggle"}`, `{kind: "yank"}`, `{kind: "move", motion}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CopyOpMsg {
    Enter,
    Exit,
    Toggle,
    Yank,
    Move { motion: CopyMotionMsg },
}

impl From<CopyOpMsg> for CopyOp {
    fn from(op: CopyOpMsg) -> Self {
        match op {
            CopyOpMsg::Enter => CopyOp::Enter,
            CopyOpMsg::Exit => CopyOp::Exit,
            CopyOpMsg::Toggle => CopyOp::ToggleSelection,
            CopyOpMsg::Yank => CopyOp::Yank,
            CopyOpMsg::Move { motion } => CopyOp::Move(motion.into()),
        }
    }
}

/// What a paste did: `written`, `empty` or `rejected`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasteOutcomeMsg {
    Written,
    Empty,
    Rejected,
}

impl From<PasteOutcome> for PasteOutcomeMsg {
    fn from(outcome: PasteOutcome) -> Self {
        match outcome {
            PasteOutcome::Written => PasteOutcomeMsg::Written,
            PasteOutcome::Empty => PasteOutcomeMsg::Empty,
            PasteOutcome::Rejected => PasteOutcomeMsg::Rejected,
        }
    }
}

/// What a pointer button did: `press`, `release` or `motion`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseActionMsg {
    Press,
    Release,
    Motion,
}

impl From<MouseActionMsg> for MouseAction {
    fn from(action: MouseActionMsg) -> Self {
        match action {
            MouseActionMsg::Press => MouseAction::Press,
            MouseActionMsg::Release => MouseAction::Release,
            MouseActionMsg::Motion => MouseAction::Motion,
        }
    }
}

/// Which button: `left`, `middle`, `right`, or `none` for a motion with nothing held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButtonMsg {
    Left,
    Middle,
    Right,
    None,
}

impl From<MouseButtonMsg> for MouseButton {
    fn from(button: MouseButtonMsg) -> Self {
        match button {
            MouseButtonMsg::Left => MouseButton::Left,
            MouseButtonMsg::Middle => MouseButton::Middle,
            MouseButtonMsg::Right => MouseButton::Right,
            MouseButtonMsg::None => MouseButton::None,
        }
    }
}

/// The state of copy mode after an operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CopyStateMsg {
    pub active: bool,
    pub selecting: bool,
    /// The text handed back by `yank`, `null` otherwise or when the selection held no text.
    pub yanked: Option<String>,
}

impl From<CopyState> for CopyStateMsg {
    fn from(state: CopyState) -> Self {
        CopyStateMsg { active: state.active, selecting: state.selecting, yanked: state.yanked }
    }
}

/// Why a file grant is refused, as a name that does not change: the page maps it to a sentence. The core's own text of a
/// refusal is not part of the protocol (it is for people, and it quotes what was sent), so the page reads this and not that.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantCode {
    /// The secrets of the person are locked, and the source needs one (a saved login).
    Locked,
    /// The file is not there (it was moved or deleted), or the path names nothing.
    NotFound,
    /// The person's own account may not read the file.
    Denied,
    /// The file is not an image, a video, audio or a PDF (or it is a folder): the only kinds the core grants.
    Kind,
    /// Any other reason; the page says it plainly.
    Other,
}

/// The answer of `file_grant_why`: the stable code of the reason, and nothing else. Never the core's text and never a
/// part of what was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct GrantWhy {
    pub code: GrantCode,
}

/// The answer of `file_grant`: the address the page loads the file from beside the grant. It serializes as the `url`
/// first and then the members of the core's grant (`token`, `mime`, `size`), as the desktop always answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GrantAnswer {
    pub url: String,
    #[serde(flatten)]
    pub grant: Grant,
}
