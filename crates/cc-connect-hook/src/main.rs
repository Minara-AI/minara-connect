//! cc-connect-hook — Claude Code `UserPromptSubmit` hook.
//!
//! Implements PROTOCOL.md §7. Always exits 0 — any non-zero exit blocks
//! the user prompt in Claude Code, which is unacceptable in v0.1. Errors
//! are written to `~/.cc-connect/hook.log` and silenced.

use anyhow::{anyhow, Context, Result};
use cc_connect_core::{cursor_io, hook_format, log_io, message::Message};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

fn main() -> ! {
    if let Err(e) = run() {
        let log = home_dir().join(".cc-connect").join("hook.log");
        let _ = append_log(&log, &format!("[{}] hook error: {e:#}\n", iso_now()));
    }
    // PROTOCOL.md §7.4: hook MUST always exit 0.
    std::process::exit(0)
}

fn run() -> Result<()> {
    // Step 1: parse stdin JSON for session_id.
    let session_id = match read_session_id() {
        Ok(s) => s,
        Err(e) => {
            // Per §7.4: missing/malformed session_id → no-op + warn.
            eprintln!("cc-connect-hook: {e}");
            return Ok(());
        }
    };

    // Step 2-3: enumerate active rooms (live PIDs only). If
    // `CC_CONNECT_ROOM` is set in the hook's environment, scope to that one
    // topic — used by the TUI/room orchestrator to bind a specific Claude
    // Code session to a specific room. Standalone Claude Code (no env var)
    // keeps the legacy "inject every active room" behaviour.
    let active_rooms_dir = active_rooms_dir()?;
    let mut topic_ids = enumerate_active_rooms(&active_rooms_dir)?;
    if let Ok(forced) = std::env::var("CC_CONNECT_ROOM") {
        topic_ids.retain(|t| t == &forced);
    }
    if topic_ids.is_empty() {
        // No (matching) active rooms → empty stdout, exit 0. Canonical
        // "no new Messages".
        return Ok(());
    }

    // Step 4-5: per active Room, read cursor + unread Messages.
    //
    // We collect (topic_id, cursor_path, messages, highest_seen_id) up front
    // so we can run the fcntl-locked reads to completion before doing any
    // stdout writes (which can fail and abort us).
    let mut rooms: HashMap<String, Vec<Message>> = HashMap::new();
    let mut cursor_advances: Vec<(PathBuf, String)> = Vec::new();

    for topic_id in &topic_ids {
        let cursor_path = cursor_path_for(topic_id, &session_id);
        let cursor = cursor_io::read_cursor(&cursor_path).unwrap_or(None);

        let log_path = log_path_for(topic_id);
        let mut log_file = match log_io::open_or_create_log(&log_path) {
            Ok(f) => f,
            Err(_) => continue, // Best-effort: missing log = nothing to inject.
        };
        let messages = match log_io::read_since(&mut log_file, cursor.as_deref()) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if let Some(highest) = messages.last().map(|m| m.id.clone()) {
            cursor_advances.push((cursor_path, highest));
        }
        rooms.insert(topic_id.clone(), messages);
    }

    // Step 6: render via cc-connect-core::hook_format. The 8 KiB iterative
    // truncation is handled internally; PROTOCOL.md §7.3 step 6 spec.
    let nicknames = read_nicknames();
    let rooms_base = home_dir().join(".cc-connect").join("rooms");
    let self_nick = read_self_nick();
    let room_summaries = read_room_summaries(&rooms_base, rooms.keys());
    let room_file_indexes = read_room_file_indexes(&rooms_base, rooms.keys());
    let body = hook_format::render(&hook_format::HookInput {
        rooms: &rooms,
        nicknames: &nicknames,
        rooms_base: &rooms_base,
        self_nick: self_nick.as_deref(),
        room_summaries: &room_summaries,
        room_file_indexes: &room_file_indexes,
    });
    // Prepend a per-prompt orientation header. Tells Claude exactly which
    // cc-connect room it's bound to + what MCP tools it has + what nick
    // peers see it as. Without this Claude is blind to its own
    // membership and asks "which room?" on every prompt.
    // Empty body = no unread chat. Stay silent (no header either) so the
    // hook doesn't spam Claude on every prompt when the room is quiet.
    let output = if body.is_empty() {
        String::new()
    } else {
        let header = build_orientation_header(&topic_ids, self_nick.as_deref());
        format!("{header}{body}")
    };

    // Step 7: write to stdout. Empty output = exit 0 (no marker, no boilerplate).
    if !output.is_empty() {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(output.as_bytes())
            .context("write hook stdout")?;
    }

    // Step 8: advance cursors *only* after stdout has been written. This
    // ensures Claude has actually received the messages before we mark them
    // as seen — if stdout fails, the cursor stays where it was so the next
    // hook fire re-injects.
    for (cursor_path, new_ulid) in cursor_advances {
        if let Err(e) = cursor_io::advance_cursor(&cursor_path, &new_ulid) {
            // Log + continue: a failed cursor advance just means the next
            // hook fire will re-inject these messages (idempotent at the
            // chat-as-substrate level — Claude will see a duplicate, but
            // never miss a message).
            let log = home_dir().join(".cc-connect").join("hook.log");
            let _ = append_log(
                &log,
                &format!(
                    "[{}] cursor advance failed for {}: {e:#}\n",
                    iso_now(),
                    cursor_path.display()
                ),
            );
        }
    }

    Ok(())
}

/// Read JSON from stdin and extract `session_id`.
///
/// PROTOCOL.md §7.2: the field name is `session_id`. Other Claude Code-
/// supplied fields are tolerated (and ignored) for forward compat.
fn read_session_id() -> Result<String> {
    #[derive(serde::Deserialize)]
    struct StdinPayload {
        session_id: String,
    }

    let mut buf = String::new();
    std::io::stdin()
        .lock()
        .read_to_string(&mut buf)
        .context("read stdin")?;
    if buf.trim().is_empty() {
        return Err(anyhow!("empty stdin — Claude Code did not pass session JSON"));
    }
    let parsed: StdinPayload = serde_json::from_str(&buf)
        .with_context(|| format!("parse stdin as JSON; got {} bytes", buf.len()))?;
    if parsed.session_id.is_empty() {
        return Err(anyhow!("session_id field is empty"));
    }
    Ok(parsed.session_id)
}

/// Resolve `${TMPDIR}/cc-connect-${UID}/active-rooms/`.
///
/// Returns `Ok(path)` even if the directory doesn't exist yet — the caller
/// will simply find no rooms.
fn active_rooms_dir() -> Result<PathBuf> {
    let uid = rustix::process::geteuid().as_raw();
    Ok(std::env::temp_dir()
        .join(format!("cc-connect-{uid}"))
        .join("active-rooms"))
}

/// List `*.active` files in the active-rooms directory; for each one,
/// validate the parent dir mode + the PID; return only the topic_ids
/// whose owning process is alive.
///
/// Stale entries (process gone) are unlinked as we go.
fn enumerate_active_rooms(dir: &Path) -> Result<Vec<String>> {
    use std::os::unix::fs::PermissionsExt;

    if !dir.exists() {
        return Ok(Vec::new());
    }

    // PROTOCOL.md §8: refuse to operate if the parent directory is a
    // symlink or has loose permissions.
    let parent_meta = std::fs::symlink_metadata(dir)
        .with_context(|| format!("lstat {}", dir.display()))?;
    if parent_meta.file_type().is_symlink() {
        return Err(anyhow!("{} is a symlink — refusing", dir.display()));
    }
    if !parent_meta.is_dir() {
        return Err(anyhow!("{} is not a directory", dir.display()));
    }
    let mode = parent_meta.permissions().mode() & 0o777;
    if mode != 0o700 {
        return Err(anyhow!(
            "{} has mode {:o} (expected 0700)",
            dir.display(),
            mode
        ));
    }

    let mut topic_ids = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let topic_id = match path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".active"))
        {
            Some(s) => s.to_string(),
            None => continue, // Not our file.
        };

        match check_pid_alive(&path) {
            Ok(true) => topic_ids.push(topic_id),
            Ok(false) => {
                let _ = std::fs::remove_file(&path); // Stale; sweep.
            }
            Err(_) => {
                let _ = std::fs::remove_file(&path); // Malformed; sweep.
            }
        }
    }
    Ok(topic_ids)
}

/// Read `<active>.active`, parse the PID (validated to ≥ 100 and ≤ i32::MAX
/// per PROTOCOL.md §8), and check the process is alive via signal-0.
fn check_pid_alive(path: &Path) -> Result<bool> {
    let raw = std::fs::read_to_string(path)?;
    let pid_str = raw.trim();
    let pid: i32 = pid_str.parse().context("PID file content not an integer")?;
    if pid < 100 || pid > i32::MAX {
        return Err(anyhow!("PID {pid} outside valid range [100, i32::MAX]"));
    }
    let pid_obj = match rustix::process::Pid::from_raw(pid) {
        Some(p) => p,
        None => return Err(anyhow!("PID {pid} rejected by rustix")),
    };
    match rustix::process::test_kill_process(pid_obj) {
        Ok(()) => Ok(true),
        Err(e) if e == rustix::io::Errno::SRCH => Ok(false),
        Err(e) => Err(anyhow!("test_kill_process({pid}): {e}")),
    }
}

fn cursor_path_for(topic_id: &str, session_id: &str) -> PathBuf {
    home_dir()
        .join(".cc-connect")
        .join("cursors")
        .join(topic_id)
        .join(format!("{session_id}.cursor"))
}

fn log_path_for(topic_id: &str) -> PathBuf {
    home_dir()
        .join(".cc-connect")
        .join("rooms")
        .join(topic_id)
        .join("log.jsonl")
}

/// Read `~/.cc-connect/nicknames.json` if it exists. Best-effort: any
/// error returns an empty map (the hook_format will fall back to pubkey
/// prefixes).
fn read_nicknames() -> HashMap<String, String> {
    let path = home_dir().join(".cc-connect").join("nicknames.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

/// Per-prompt orientation block. Tells Claude what room it's in, what
/// nick peers see, and which MCP tools exist. Without it Claude has to
/// guess from the chat lines alone — and often guesses wrong.
fn build_orientation_header(topic_ids: &[String], self_nick: Option<&str>) -> String {
    if topic_ids.is_empty() {
        return String::new();
    }
    let nick_line = match self_nick {
        Some(n) if !n.is_empty() => format!("you (this Claude) = {n}"),
        _ => "you (this Claude) = anonymous (no self_nick set)".to_string(),
    };
    let topics_line = topic_ids
        .iter()
        .map(|t| t.chars().take(12).collect::<String>())
        .collect::<Vec<_>>()
        .join(", ");
    let mut s = String::new();
    s.push_str("[cc-connect] active room context\n");
    s.push_str(&format!("  topics: {topics_line}\n"));
    s.push_str(&format!("  {nick_line}\n"));
    s.push_str("  MCP tools you can call: cc_send(body), cc_at(nick, body), cc_drop(path), cc_recent(limit), cc_list_files(limit), cc_save_summary(text)\n");
    s.push_str("  Lines below tagged [chatroom …] are unread chat messages from peers; mention them if relevant before answering the user.\n\n");
    s
}

/// For each active topic, read `<rooms_base>/<topic>/summary.md` if it
/// exists. Best-effort: missing / unreadable files just produce no entry.
fn read_room_summaries<'a, I: Iterator<Item = &'a String>>(
    rooms_base: &Path,
    topics: I,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for topic in topics {
        let path = rooms_base.join(topic).join("summary.md");
        if let Ok(s) = std::fs::read_to_string(&path) {
            if !s.trim().is_empty() {
                out.insert(topic.clone(), s);
            }
        }
    }
    out
}

/// For each active topic, read `<rooms_base>/<topic>/files/INDEX.md` if it
/// exists. The hook_format renderer trims to a tail-byte budget.
fn read_room_file_indexes<'a, I: Iterator<Item = &'a String>>(
    rooms_base: &Path,
    topics: I,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for topic in topics {
        let path = rooms_base.join(topic).join("files").join("INDEX.md");
        if let Ok(s) = std::fs::read_to_string(&path) {
            if !s.trim().is_empty() {
                out.insert(topic.clone(), s);
            }
        }
    }
    out
}

/// Read `~/.cc-connect/config.json::self_nick`. Returns `None` on any
/// error; the hook will still tag `@cc` / `@claude` / `@all` / `@here`
/// mentions even without self_nick.
fn read_self_nick() -> Option<String> {
    let path = home_dir().join(".cc-connect").join("config.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("self_nick")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn append_log(path: &Path, msg: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(msg.as_bytes())
}

/// Tiny ISO-8601-ish timestamp for hook.log lines, no chrono dep.
fn iso_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("ts={secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// active-rooms enumeration tolerates a missing directory.
    #[test]
    fn enumerate_missing_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let nonexistent = dir.path().join("never-created");
        let result = enumerate_active_rooms(&nonexistent).unwrap();
        assert!(result.is_empty());
    }

    /// enumerate_active_rooms refuses a wrong-mode parent dir.
    #[test]
    fn enumerate_rejects_loose_dir_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("active-rooms");
        std::fs::create_dir(&active).unwrap();
        std::fs::set_permissions(&active, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = enumerate_active_rooms(&active).err().unwrap();
        assert!(err.to_string().contains("0700") || err.to_string().contains("755"));
    }

    /// PID validation rejects 0, 1, negatives, garbage.
    #[test]
    fn pid_validation_rejects_invalid() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("active-rooms");
        std::fs::create_dir(&active).unwrap();
        std::fs::set_permissions(&active, std::fs::Permissions::from_mode(0o700)).unwrap();

        for (name, content) in &[
            ("zero.active", "0"),
            ("one.active", "1"),
            ("negative.active", "-42"),
            ("garbage.active", "not-a-pid"),
        ] {
            std::fs::write(active.join(name), content).unwrap();
        }
        let result = enumerate_active_rooms(&active).unwrap();
        assert!(
            result.is_empty(),
            "all bogus PIDs MUST be swept, got: {result:?}"
        );
    }

    /// Live PID for our own test process is detected as alive.
    #[test]
    fn pid_detection_finds_self() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let active = dir.path().join("active-rooms");
        std::fs::create_dir(&active).unwrap();
        std::fs::set_permissions(&active, std::fs::Permissions::from_mode(0o700)).unwrap();
        let topic = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4";
        std::fs::write(
            active.join(format!("{topic}.active")),
            std::process::id().to_string(),
        )
        .unwrap();
        let result = enumerate_active_rooms(&active).unwrap();
        assert_eq!(result, vec![topic.to_string()]);
    }

    /// nicknames missing → empty map, no panic.
    #[test]
    fn read_nicknames_tolerates_missing_file() {
        // Just exercises the function; we can't easily redirect HOME for a
        // unit test, but the helper falls back to {} on any error.
        let _ = read_nicknames();
    }
}
