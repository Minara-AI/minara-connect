//! Consent-gate accept path — `cc-connect accept <token>`.
//!
//! See `PROTOCOL.md` §7.3 step 0 / `SECURITY.md` §3 for the threat
//! model. When a Claude calls the `cc_join_room` MCP tool, the MCP
//! server files a pending-join at
//! `~/.cc-connect/pending-joins/<token>.json` instead of immediately
//! binding that Claude to the Room. The human reviews the pending join
//! in the side-channel viewer (`cc-connect watch` or VSCode panel) and
//! runs this command (or clicks Accept) to actually consent.
//!
//! The flow:
//!   1. `session_state::consume_pending_join(token)` atomically
//!      read-and-deletes the pending file (so a second accept attempt
//!      fails clean).
//!   2. We re-ensure the chat-daemon is running for the topic — if the
//!      daemon crashed between `cc_join_room` and `cc-connect accept`,
//!      it gets re-spawned here. Idempotent.
//!   3. `session_state::add_topic(claude_pid, topic)` adds the topic to
//!      the requesting Claude's `rooms.json`. The next time that Claude
//!      submits a prompt, the hook will inject this Room's chat.
//!
//! `claude_pid` comes from the pending-join file (recorded by the MCP
//! server via `claude_pid::find_claude_ancestor`). Even if the user
//! happens to invoke `cc-connect accept` from a different terminal, the
//! consent applies to the correct Claude session.
//!
//! Also exports `run_pending_list` for the watch UI / debug surfaces.

use anyhow::{Context, Result};
use cc_connect_core::session_state::{self, PendingJoin};

/// `cc-connect accept <token>` — consume the pending-join and bind
/// the requesting Claude to the room.
pub fn run_accept(token: &str) -> Result<()> {
    let pj: PendingJoin = session_state::consume_pending_join(token)
        .with_context(|| format!("consume pending-join token `{token}`"))?;

    // Re-ensure the chat-daemon. The MCP server already spawned one when
    // `cc_join_room` was called, but the daemon may have crashed in the
    // window between request and accept. `chat_daemon::run_start` is
    // idempotent — `ALREADY` print + exit 0 if the daemon is alive.
    crate::chat_daemon::run_start(&pj.ticket, /*no_relay=*/ false, None)
        .with_context(|| format!("ensure chat-daemon for topic {}", short(&pj.topic)))?;

    // Bind the topic to the requesting Claude's rooms.json. From this
    // point the hook will inject this Room's chat into that Claude's
    // next prompt.
    session_state::add_topic(pj.claude_pid, &pj.topic)
        .with_context(|| format!("add_topic({}) for claude_pid {}", &pj.topic, pj.claude_pid))?;

    println!(
        "Bound claude pid {} to room {} (ticket {:.20}…).",
        pj.claude_pid,
        short(&pj.topic),
        pj.ticket
    );
    println!("The hook will inject this Room's chat on the next prompt that Claude submits.");
    Ok(())
}

/// `cc-connect pending-list` — print every pending-join awaiting human
/// consent. Used by the watch UI and for debugging.
pub fn run_pending_list() -> Result<()> {
    let mut joins = session_state::list_pending_joins()?;
    joins.sort_by_key(|p| p.requested_at_ms);

    if joins.is_empty() {
        println!("(no pending joins)");
        return Ok(());
    }

    println!("{:<34}  {:<8}  {:<14}  ticket", "token", "pid", "topic");
    for pj in joins {
        println!(
            "{:<34}  {:<8}  {:<14}  {:.40}…",
            pj.token,
            pj.claude_pid,
            short(&pj.topic),
            pj.ticket
        );
    }
    Ok(())
}

fn short(topic: &str) -> String {
    topic.chars().take(12).collect()
}
