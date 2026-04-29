//! cc-connect-tui — vertical-split TUI hosting a chat session and an
//! embedded Claude Code (PTY) child.
//!
//! Public entry: [`run`]. The caller (`crates/cc-connect/src/room.rs`)
//! sets up the chat ticket and spawns this in `cc-connect room start /
//! join`.

pub mod app;
pub mod chat_pane;
pub mod claude_pane;
pub mod event_loop;
pub mod mention;
pub mod setup;
pub mod tabs;
pub mod theme;

pub use event_loop::{run, RunOpts};
