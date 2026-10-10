//! A pseudo-terminal running a child process, on top of `portable-pty`.

use std::io::{Read, Write};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::{Error, Result};

fn pty<E: std::fmt::Display>(e: E) -> Error {
    Error::Pty(e.to_string())
}

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    reader: Option<Box<dyn Read + Send>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Pty {
    /// Start `program` with `args` in a new PTY of the given size.
    pub fn spawn(program: &str, args: &[&str], cols: u16, rows: u16) -> Result<Self> {
        let pair = native_pty_system()
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(pty)?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(cmd).map_err(pty)?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(pty)?;
        let writer = pair.master.take_writer().map_err(pty)?;
        Ok(Self { master: pair.master, writer, reader: Some(reader), child })
    }

    /// The read end. It can be taken once, to move it to a reader thread.
    pub fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        self.reader.take()
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// The operating system's id of the child, while it is known.
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// End the child, reap it and release the PTY. Returns the child's exit code.
    ///
    /// A child that has not exited gets SIGHUP, a short grace period and then SIGKILL (portable-pty's
    /// `Child::kill`); `close` always waits afterwards, so no zombie remains, even for a shell that ignores
    /// SIGHUP. The writer and the master are dropped only after the child is gone, in that order: the writer's
    /// `Drop` writes a newline and EOF into the PTY, which would answer a live program's prompt.
    ///
    /// This blocks for up to about 200 ms while the child is given time to leave. Never call it on the Tauri main
    /// thread.
    pub fn close(mut self) -> Result<u32> {
        // A child that was already reaped must not be signalled: its pid may belong to someone else by now.
        let exited = self.child.try_wait().ok().flatten();
        let outcome = match exited {
            Some(status) => Ok(status.exit_code()),
            None => {
                let _ = self.child.kill();
                self.child.wait().map(|status| status.exit_code())
            }
        };
        // The child is gone (or could not be waited for): only now release the PTY.
        drop(self.writer);
        drop(self.master);
        drop(self.reader);
        Ok(outcome?)
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(pty)
    }

    /// Wait for the child to exit and return its exit code.
    pub fn wait(&mut self) -> Result<u32> {
        Ok(self.child.wait()?.exit_code())
    }
}
