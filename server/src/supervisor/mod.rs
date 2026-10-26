//! The supervisor: the network-facing process that starts, tracks and stops one worker per person.

pub mod fanout;
pub mod helper_client;
pub mod idle;
pub mod launcher;
pub mod registry;
pub mod socket;
