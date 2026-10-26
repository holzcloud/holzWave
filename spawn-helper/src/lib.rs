#![deny(unsafe_code)]
//! The privileged helper of the holzWave server: the only process that holds file capabilities (Linux only).
//!
//! The network-facing supervisor holds no capability. It starts this helper once, with a `SOCK_SEQPACKET` socket as the
//! helper's standard input, and asks it to create the home of a person, to start the person's worker under that
//! person's user and group, to signal the processes of a person and to remove a home. The helper trusts nothing in a
//! request: the user range, the home base, the worker program, the shell and the limits all come from its own
//! arguments, which [`args`] checks at start.
//!
//! The pure rules ([`args`], [`protocol`], [`validate`] and the wording in [`caps`]) build everywhere; the
//! parts that talk to the Linux kernel are compiled on Linux only.

pub mod args;
pub mod caps;
pub mod protocol;
pub mod validate;

#[cfg(target_os = "linux")]
pub mod home;
#[cfg(target_os = "linux")]
pub mod spawn;
