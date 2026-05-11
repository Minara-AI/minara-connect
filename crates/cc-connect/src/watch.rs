//! `cc-connect watch` — human side-channel viewer for the MCP-first
//! model (ADR-0005).
//!
//! Two responsibilities:
//!
//!   1. **Surface pending `cc_join_room` requests** so the human can
//!      run `cc-connect accept <token>`. The MCP server's consent gate
//!      (ADR-0006) is a no-op until the human sees the request and
//!      acts; this command is the canonical side-channel surface for
//!      that.
//!
//!   2. **Tail the chat log** of every Room any of this user's
//!      running Claudes are bound to. In MCP-first mode the human is
//!      no longer typing in a chat pane, but they still want
//!      visibility into what their Claude is saying / hearing.
//!
//! Polling cadence: 1.5s. Plain stdout — no TUI dep, no per-platform
//! terminal contortions. Run it in a side terminal or tmux pane and
//! forget about it. Stop with Ctrl-C; this command never exits on its
//! own.

use anyhow::{anyhow, Context, Result};
use cc_connect_core::{log_io, message, message::Message, session_state};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_millis(1500);

/// On the very first sight of a Room, print this many trailing chat
/// lines so the human has context — not just whatever message lands
/// next. Picked to match the hook's typical injection budget so the
/// watch terminal and Claude's prompt see roughly the same window.
const INITIAL_TAIL_LINES: usize = 10;

pub fn run_watch() -> Result<()> {
    println!(
        "[cc-connect watch] polling pending-joins + bound rooms every {}ms — Ctrl-C to stop",
        POLL_INTERVAL.as_millis()
    );
    println!();

    let mut last_id_per_topic: HashMap<String, String> = HashMap::new();
    let mut announced_pending: HashSet<String> = HashSet::new();
    let mut last_topics: BTreeSet<String> = BTreeSet::new();

    loop {
        if let Err(e) = poll_pending_joins(&mut announced_pending) {
            eprintln!("[watch] pending-joins error: {e:#}");
        }

        let topics = match collect_bound_topics() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[watch] read sessions: {e:#}");
                BTreeSet::new()
            }
        };
        diff_topic_set(&last_topics, &topics);
        last_topics = topics.clone();

        for topic in &topics {
            if let Err(e) = tail_topic(topic, &mut last_id_per_topic) {
                eprintln!("[watch] tail {}: {:#}", short(topic), e);
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

fn poll_pending_joins(announced: &mut HashSet<String>) -> Result<()> {
    let pending = session_state::list_pending_joins()?;
    let current: HashSet<String> = pending.iter().map(|p| p.token.clone()).collect();

    for pj in &pending {
        if announced.insert(pj.token.clone()) {
            print_pending_join(pj);
        }
    }
    // Forget tokens that no longer exist on disk so a future request
    // re-using the (random) token would still re-announce.
    announced.retain(|t| current.contains(t));
    Ok(())
}

fn print_pending_join(pj: &session_state::PendingJoin) {
    println!();
    println!("┌── pending cc_join_room ──────────────────────────────────");
    println!("│ token:      {}", pj.token);
    println!("│ claude pid: {}", pj.claude_pid);
    println!("│ topic:      {}", short(&pj.topic));
    println!("│ ticket:     {:.40}…", pj.ticket);
    println!("│ → run:      cc-connect accept {}", pj.token);
    println!("└──────────────────────────────────────────────────────────");
    println!();
}

fn diff_topic_set(prev: &BTreeSet<String>, now: &BTreeSet<String>) {
    for added in now.difference(prev) {
        println!("[watch] room {} now bound", short(added));
    }
    for removed in prev.difference(now) {
        println!(
            "[watch] room {} no longer bound by any Claude",
            short(removed)
        );
    }
}

/// Walk `~/.cc-connect/sessions/by-claude-pid/*/rooms.json` and union
/// every topic referenced. We don't need to know which Claude owns
/// which topic — for tailing purposes a Room is interesting if any
/// alive Claude is in it.
fn collect_bound_topics() -> Result<BTreeSet<String>> {
    let root = sessions_root()?;
    if !root.exists() {
        return Ok(BTreeSet::new());
    }
    #[derive(Deserialize)]
    struct RoomsView {
        topics: Vec<String>,
    }
    let mut topics = BTreeSet::new();
    for entry in std::fs::read_dir(&root)
        .with_context(|| format!("read_dir {}", root.display()))?
        .flatten()
    {
        let rooms_json = entry.path().join("rooms.json");
        let Ok(raw) = std::fs::read_to_string(&rooms_json) else {
            continue;
        };
        let Ok(view) = serde_json::from_str::<RoomsView>(&raw) else {
            continue;
        };
        topics.extend(view.topics);
    }
    Ok(topics)
}

fn tail_topic(topic: &str, last_id_per_topic: &mut HashMap<String, String>) -> Result<()> {
    let log_path = log_path_for(topic)?;
    if !log_path.exists() {
        return Ok(());
    }
    let mut file = log_io::open_or_create_log(&log_path)?;
    let cursor = last_id_per_topic.get(topic).cloned();
    let unread = log_io::read_since(&mut file, cursor.as_deref())?;
    if unread.is_empty() {
        return Ok(());
    }

    let to_print: &[Message] = if cursor.is_none() {
        let start = unread.len().saturating_sub(INITIAL_TAIL_LINES);
        &unread[start..]
    } else {
        &unread
    };
    for msg in to_print {
        if msg.kind == message::KIND_KEEPALIVE {
            // PROTOCOL §4: keepalives are mesh heartbeats, not chat. The
            // log shouldn't contain them, but be defensive.
            continue;
        }
        print_message(topic, msg);
    }
    if let Some(last) = unread.last() {
        last_id_per_topic.insert(topic.to_string(), last.id.clone());
    }
    Ok(())
}

fn print_message(topic: &str, msg: &Message) {
    let when = format_ts(msg.ts);
    let who = msg
        .nick
        .clone()
        .unwrap_or_else(|| short_author(&msg.author));
    let body = match msg.kind.as_str() {
        message::KIND_CHAT => msg.body.clone(),
        message::KIND_FILE_DROP => format!(
            "[file_drop] {} ({} bytes)",
            msg.body,
            msg.blob_size.unwrap_or(0)
        ),
        other => format!("[{other}] {}", msg.body),
    };
    println!("[{when}] ({}) {who}: {body}", short(topic), body = body);
}

fn format_ts(ts_ms: i64) -> String {
    // UTC HH:MM:SS — no extra deps. The watch is operator-facing and
    // running in the background; full date is noise.
    let total_secs = (ts_ms.max(0) as u64) / 1000;
    let secs_in_day = total_secs % 86_400;
    let h = secs_in_day / 3600;
    let m = (secs_in_day / 60) % 60;
    let s = secs_in_day % 60;
    format!("{h:02}:{m:02}:{s:02}Z")
}

fn short(hex: &str) -> String {
    hex.chars().take(12).collect()
}

fn short_author(author: &str) -> String {
    // Author is a 52-char base32 NodeId; show the first 8 so it fits.
    author.chars().take(8).collect()
}

fn sessions_root() -> Result<PathBuf> {
    Ok(home_dir()?
        .join(".cc-connect")
        .join("sessions")
        .join("by-claude-pid"))
}

fn log_path_for(topic: &str) -> Result<PathBuf> {
    Ok(home_dir()?
        .join(".cc-connect")
        .join("rooms")
        .join(topic)
        .join("log.jsonl"))
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME env var not set"))
}
