//! The shared support of the security corpora: canaries, hostile text, the service rig and the tool world.
//!
//! Every gate file declares `mod common;` and then `mod security;`. Nothing here is compiled into a product, and no
//! secret is written whole in any of these files: each canary is assembled at run time from a prefix and a body, and
//! each hidden character is made with `char::from_u32`.

// Each gate file uses a subset of the support, so what a file does not use is not dead code.
#![allow(dead_code)]

pub mod canaries;
pub mod hostile;
pub mod rig;
