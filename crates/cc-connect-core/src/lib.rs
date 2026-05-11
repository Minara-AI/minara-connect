//! cc-connect-core — shared types and on-disk I/O for cc-connect.
//!
//! See `PROTOCOL.md` at the repository root for the wire and on-disk
//! specification this crate implements.

pub mod claude_pid;
pub mod cursor_io;
pub mod drop_safety;
pub mod hook_format;
pub mod identity;
pub mod log_io;
pub mod message;
pub mod posix;
pub mod rate_limit;
pub mod session_state;
pub mod ticket;
