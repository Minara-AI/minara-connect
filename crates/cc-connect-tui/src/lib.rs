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
pub mod tabs;
pub mod theme;

// Setup wizard moved to `cc_connect::setup` so the room launcher
// (`cc-connect room start/join`, in the cc-connect crate) can run it
// before deciding whether to spawn a multiplexer or fall back to this
// TUI. cc-connect-tui's main re-exports it under the old path for any
// caller that imported `cc_connect_tui::setup`.
pub use cc_connect::setup;

pub use event_loop::{run, RunOpts};
