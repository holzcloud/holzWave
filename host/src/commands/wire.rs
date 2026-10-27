//! The wire types of the arguments and answers of the commands: what the page sends and reads, as serde names it. The
//! page tests and the protocol golden pin the spelling, so a name here never changes without both.

use holzwave_core::engine::PasteOutcome;
use serde::{Deserialize, Serialize};

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
