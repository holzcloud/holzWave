//! What a pane talks to: the [`Link`] trait, one implementation per kind of connection.
//!
//! A pane does not care whether its bytes come from a shell on a pseudo terminal ([`crate::pty::Pty`]) or from a
//! shell on another machine ([`crate::ssh::SshLink`]): it reads output, writes input, tells the far side its size,
//! asks whether the far side has ended and, at the end, closes it. That is this trait. Everything else about a
//! connection (host, key, reconnect) is in the connection's own type and in its [`ConnState`].

use std::io::{Read, Write};
use std::sync::{Arc, Mutex, PoisonError};

use crate::Result;

/// The state of a connection as the user sees it (pane header, sidebar, status line).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConnState {
    /// A local shell, or a link that has no state of its own.
    #[default]
    Local,
    /// The first connection is being made.
    Connecting,
    /// Connected, and the shell is running.
    Connected,
    /// The connection was lost and is being made again: attempt `attempt` of `of`, the next one in `in_secs` seconds.
    Reconnecting { attempt: u32, of: u32, in_secs: u64 },
    /// The connection ended and will not be made again.
    Closed,
}

/// A state shared between the thread that runs a connection and the code that shows it.
#[derive(Clone, Debug, Default)]
pub struct SharedState(Arc<Mutex<ConnState>>);

impl SharedState {
    pub fn new(state: ConnState) -> Self {
        Self(Arc::new(Mutex::new(state)))
    }

    pub fn get(&self) -> ConnState {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn set(&self, state: ConnState) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = state;
    }
}

/// The far side of a pane.
pub trait Link: Send {
    /// The read end, taken once to move it to the pane's reader thread. `None` after that.
    fn take_reader(&mut self) -> Option<Box<dyn Read + Send>>;

    /// The write end, taken once to move it to the pane's writer thread. `None` after that.
    fn take_writer(&mut self) -> Option<Box<dyn Write + Send>>;

    /// The operating system's id of the local process, if there is one. A remote link has none.
    fn process_id(&self) -> Option<u32>;

    /// Tell the far side the size of the grid in cells and, if known, in pixels.
    fn resize_px(&self, cols: u16, rows: u16, px_w: u32, px_h: u32) -> Result<()>;

    /// The exit code if the far side has ended, `None` while it runs. Does not block.
    fn try_wait(&mut self) -> Result<Option<u32>>;

    /// End the far side and release everything. Returns the exit code. May block for a short time.
    fn close(self: Box<Self>) -> Result<u32>;

    /// The state of the connection, for display.
    fn state(&self) -> ConnState {
        ConnState::Local
    }
}

impl Link for crate::pty::Pty {
    fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        crate::pty::Pty::take_reader(self)
    }

    fn take_writer(&mut self) -> Option<Box<dyn Write + Send>> {
        crate::pty::Pty::take_writer(self)
    }

    fn process_id(&self) -> Option<u32> {
        crate::pty::Pty::process_id(self)
    }

    fn resize_px(&self, cols: u16, rows: u16, px_w: u32, px_h: u32) -> Result<()> {
        crate::pty::Pty::resize_px(self, cols, rows, px_w, px_h)
    }

    fn try_wait(&mut self) -> Result<Option<u32>> {
        crate::pty::Pty::try_wait(self)
    }

    fn close(self: Box<Self>) -> Result<u32> {
        crate::pty::Pty::close(*self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_state_is_seen_by_every_clone() {
        let a = SharedState::new(ConnState::Connecting);
        let b = a.clone();
        b.set(ConnState::Reconnecting { attempt: 2, of: 5, in_secs: 3 });
        assert_eq!(a.get(), ConnState::Reconnecting { attempt: 2, of: 5, in_secs: 3 });
    }
}
