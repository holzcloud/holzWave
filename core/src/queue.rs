//! The bounded per-pane queue between the PTY reader thread and the engine thread: a fast producer is slowed down
//! instead of growing memory without limit.
