//! The pane actor: one engine thread that owns the engine and the PTY writer, a PTY reader thread feeding it, and
//! the pacing of frames towards the shell.
