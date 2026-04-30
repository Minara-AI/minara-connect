//! Shared per-launch tmpfile staging for the room launcher and the embedded
//! TUI.
//!
//! The zellij + tmux + embedded-TUI paths all spawn `claude` through
//! `layouts/claude-wrap.sh` so they agree on:
//!   - how `CC_CONNECT_ROOM` gets exported into the child env
//!   - whether `--append-system-prompt` (auto-reply) is set
//!   - whether the bootstrap user-prompt is appended as the first turn
//!   - whether `--permission-mode bypassPermissions` is added by default
//!
//! The wrapper script + prompt files are checked-in markdown / sh; we
//! `include_str!` them here and write them into `/tmp/cc-connect-<uid>/`
//! at launch time so callers (zellij KDL, tmux script, TUI) just spawn
//! `wrapper-path topic_hex …`.
//!
//! Lives in the cc-connect library (not in `room.rs`) so the
//! `cc-connect-tui` binary, which depends on this crate, can use the same
//! prep helpers without re-implementing them. CLAUDE.md "Domain-first
//! naming": these are launcher staging paths, not generic tmpfile helpers.

use anyhow::{Context, Result};
use cc_connect_core::posix::cc_connect_uid_dir;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Wrapper shell script — same source the zellij KDL and tmux script
/// reference. Written to `/tmp/cc-connect-<uid>/claude-wrap.sh` on each
/// call to [`prepare_claude_wrapper`].
const CLAUDE_WRAPPER_SH: &str = include_str!("../../../layouts/claude-wrap.sh");
const AUTO_REPLY_PROMPT: &str = include_str!("../../../layouts/auto-reply-prompt.md");
const BOOTSTRAP_PROMPT: &str = include_str!("../../../layouts/bootstrap-prompt.md");

/// Write `claude-wrap.sh` into the per-UID tmp dir and return the path.
/// Idempotent: every call rewrites the script bytes, so a freshly-built
/// cc-connect always swaps in its own wrapper version.
pub fn prepare_claude_wrapper() -> Result<PathBuf> {
    let dir = ensure_tmp_dir()?;
    let path = dir.join("claude-wrap.sh");
    std::fs::write(&path, CLAUDE_WRAPPER_SH)
        .with_context(|| format!("write {}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
    Ok(path)
}

/// Write the auto-reply system-prompt file unless the user has opted out
/// via `CC_CONNECT_NO_AUTO_REPLY=1`. `Ok(None)` on opt-out — the wrapper
/// then falls through to plain claude.
pub fn prepare_auto_reply_prompt() -> Result<Option<PathBuf>> {
    write_optional("auto-reply.md", AUTO_REPLY_PROMPT)
}

/// Write the bootstrap user-prompt file (the "say hello + enter listener
/// loop" first turn). Same opt-out as the auto-reply file.
pub fn prepare_bootstrap_prompt() -> Result<Option<PathBuf>> {
    write_optional("bootstrap.md", BOOTSTRAP_PROMPT)
}

fn write_optional(filename: &str, content: &str) -> Result<Option<PathBuf>> {
    if std::env::var_os("CC_CONNECT_NO_AUTO_REPLY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return Ok(None);
    }
    let dir = ensure_tmp_dir()?;
    let path = dir.join(filename);
    std::fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Ok(Some(path))
}

/// `/tmp/cc-connect-<uid>/` (or `$TMPDIR` equivalent on macOS), created
/// at mode 0700 if missing. Shared with chat_session's active-rooms
/// directory parent — same per-UID root.
fn ensure_tmp_dir() -> Result<PathBuf> {
    let dir = cc_connect_uid_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("create_dir_all {}", dir.display()))?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    Ok(dir)
}
