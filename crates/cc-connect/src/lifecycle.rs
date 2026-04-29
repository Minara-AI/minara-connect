//! `cc-connect clear` and `cc-connect uninstall` — the wind-down commands.
//!
//! `clear` stops all running cc-connect background processes
//! (chat-daemons + host-bg daemons). The `--purge` flag also wipes
//! `~/.cc-connect/rooms/`, so the next room start truly starts from
//! scratch (e.g. when a stuck daemon left a corrupted log.jsonl behind).
//!
//! `uninstall` reverses what `install.sh` did:
//!   1. `clear` — stop everything
//!   2. strip the cc-connect-hook entry from `~/.claude/settings.json`
//!   3. strip the cc-connect MCP server entry from `~/.claude.json`
//!   4. remove the `~/.local/bin/cc-connect{,-hook,-mcp,-tui}` and
//!      `cc-chat-ui` symlinks
//!
//! With `--purge` it also removes `~/.cc-connect/` (identity, nicknames,
//! every room) for a complete factory reset.
//!
//! Both commands are best-effort: they log every step and continue past
//! per-step failures so a half-broken install can still be cleaned up.

use anyhow::{Context, Result};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Names of every binary `install.sh` symlinks into `~/.local/bin`.
const INSTALLED_BIN_NAMES: &[&str] = &[
    "cc-connect",
    "cc-connect-hook",
    "cc-connect-mcp",
    "cc-connect-tui",
    "cc-chat-ui",
];

/// MCP server key written by `setup::install_mcp_in_claude_json` and the
/// `claude mcp add` CLI. Same string both paths use.
const MCP_SERVER_KEY: &str = "cc-connect";

pub fn run_clear(purge: bool) -> Result<()> {
    eprintln!("[clear] stopping running daemons");

    let mut stopped = 0usize;
    let mut errors: Vec<String> = Vec::new();

    match crate::chat_daemon::list_running() {
        Ok(daemons) if daemons.is_empty() => eprintln!("  (no chat-daemons running)"),
        Ok(daemons) => {
            for d in daemons {
                let short = topic_short(&d.topic_hex);
                eprintln!("  stopping chat-daemon: {short} (pid {})", d.pid);
                if let Err(e) = crate::chat_daemon::run_stop(&d.topic_hex) {
                    errors.push(format!("chat-daemon {short}: {e:#}"));
                } else {
                    stopped += 1;
                }
            }
        }
        Err(e) => errors.push(format!("list chat-daemons: {e:#}")),
    }

    match crate::host_bg::list_running() {
        Ok(daemons) if daemons.is_empty() => eprintln!("  (no host-bg daemons running)"),
        Ok(daemons) => {
            for d in daemons {
                let short = topic_short(&d.topic_hex);
                eprintln!("  stopping host-bg: {short} (pid {})", d.pid);
                if let Err(e) = crate::host_bg::run_stop(&d.topic_hex) {
                    errors.push(format!("host-bg {short}: {e:#}"));
                } else {
                    stopped += 1;
                }
            }
        }
        Err(e) => errors.push(format!("list host-bg: {e:#}")),
    }

    eprintln!("[clear] stopped {stopped} daemon(s)");

    if purge {
        let rooms_dir = home_dir().join(".cc-connect").join("rooms");
        if rooms_dir.exists() {
            std::fs::remove_dir_all(&rooms_dir)
                .with_context(|| format!("rm -rf {}", rooms_dir.display()))?;
            eprintln!("[clear] purged {}", rooms_dir.display());
        } else {
            eprintln!(
                "[clear] no rooms directory at {} to purge",
                rooms_dir.display()
            );
        }
    }

    if !errors.is_empty() {
        eprintln!("[clear] errors during shutdown:");
        for e in errors {
            eprintln!("  - {e}");
        }
    }

    eprintln!(
        "[clear] done. Restart Claude Code if you want it to drop any \
         now-stale cc-connect-mcp child."
    );
    Ok(())
}

pub fn run_uninstall(purge: bool) -> Result<()> {
    eprintln!("[uninstall] cc-connect");

    eprintln!("[uninstall] step 1/4: stopping daemons");
    if let Err(e) = run_clear(false) {
        // Treat clear failures as warnings — uninstall must continue.
        eprintln!("  warn: clear failed: {e:#}");
    }

    eprintln!("[uninstall] step 2/4: removing hook entry from ~/.claude/settings.json");
    if let Err(e) = remove_hook_from_settings() {
        eprintln!("  warn: {e:#}");
    }

    eprintln!("[uninstall] step 3/4: removing MCP server entry from ~/.claude.json");
    if let Err(e) = remove_mcp_from_claude_json() {
        eprintln!("  warn: {e:#}");
    }
    if let Err(e) = remove_mcp_via_claude_cli() {
        eprintln!("  warn: claude mcp remove: {e:#}");
    }

    eprintln!("[uninstall] step 4/4: removing ~/.local/bin symlinks");
    let bin_dir = home_dir().join(".local").join("bin");
    let mut removed = 0usize;
    for name in INSTALLED_BIN_NAMES {
        let link = bin_dir.join(name);
        match std::fs::symlink_metadata(&link) {
            Ok(meta) if meta.file_type().is_symlink() => {
                if let Err(e) = std::fs::remove_file(&link) {
                    eprintln!("  warn: rm {}: {e}", link.display());
                } else {
                    eprintln!("  removed {}", link.display());
                    removed += 1;
                }
            }
            Ok(_) => {
                eprintln!(
                    "  skipped {}: not a symlink (won't touch a real file)",
                    link.display()
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("  warn: stat {}: {e}", link.display()),
        }
    }
    if removed == 0 {
        eprintln!("  (no cc-connect symlinks to remove)");
    }

    let cc_dir = home_dir().join(".cc-connect");
    if purge {
        if cc_dir.exists() {
            std::fs::remove_dir_all(&cc_dir)
                .with_context(|| format!("rm -rf {}", cc_dir.display()))?;
            eprintln!(
                "[uninstall] purged {} (identity + nicknames + rooms)",
                cc_dir.display()
            );
        }
    } else if cc_dir.exists() {
        eprintln!(
            "[uninstall] kept {} — re-run with --purge to wipe identity + nicknames + rooms",
            cc_dir.display()
        );
    }

    eprintln!();
    eprintln!("[uninstall] done.");
    eprintln!("  • Restart Claude Code so it drops the now-stale MCP server child.");
    eprintln!("  • To reinstall fresh: run `./install.sh` from your cc-connect clone.");
    Ok(())
}

// ---- helpers ----------------------------------------------------------------

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn topic_short(topic_hex: &str) -> &str {
    &topic_hex[..12.min(topic_hex.len())]
}

/// Strip every `UserPromptSubmit` hook entry whose command path contains
/// `cc-connect-hook` from `~/.claude/settings.json`. Writes a `.json.bak`
/// alongside before mutating.
fn remove_hook_from_settings() -> Result<()> {
    let path = home_dir().join(".claude").join("settings.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("  no settings.json — nothing to remove");
            return Ok(());
        }
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", path.display())),
    };

    let mut json: serde_json::Value = serde_json::from_str(&raw).context("parse settings.json")?;

    let removed = strip_cc_connect_hook(&mut json);
    if removed > 0 {
        let backup = path.with_extension(format!("json.bak.{}", now_secs()));
        let _ = std::fs::copy(&path, &backup);
        let written = serde_json::to_string_pretty(&json)? + "\n";
        std::fs::write(&path, written).context("write settings.json")?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        eprintln!(
            "  removed {removed} hook entry/entries (backup: {})",
            backup.display()
        );
    } else {
        eprintln!("  no cc-connect-hook entries found");
    }
    Ok(())
}

/// Pure mutation: walks `hooks.UserPromptSubmit[].hooks[]` and drops every
/// inner-hook whose command contains the literal string `cc-connect-hook`.
/// Outer entries that end up with an empty `hooks` array are dropped too;
/// likewise empty `UserPromptSubmit` and empty `hooks` keys are removed.
/// Returns the count of removed inner-hook entries.
fn strip_cc_connect_hook(json: &mut serde_json::Value) -> usize {
    let mut count = 0;
    let Some(root) = json.as_object_mut() else {
        return 0;
    };
    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return 0;
    };
    if let Some(prompts) = hooks
        .get_mut("UserPromptSubmit")
        .and_then(|p| p.as_array_mut())
    {
        prompts.retain_mut(|entry| {
            if let Some(arr) = entry.get_mut("hooks").and_then(|x| x.as_array_mut()) {
                let before = arr.len();
                arr.retain(|h| {
                    let cmd = h.get("command").and_then(|c| c.as_str()).unwrap_or("");
                    !cmd.contains("cc-connect-hook")
                });
                count += before - arr.len();
                !arr.is_empty()
            } else {
                // Legacy flat shape: {command: "...cc-connect-hook"} at the
                // entry level (older install.sh). Drop the whole entry.
                let cmd = entry.get("command").and_then(|c| c.as_str()).unwrap_or("");
                if cmd.contains("cc-connect-hook") {
                    count += 1;
                    false
                } else {
                    true
                }
            }
        });
        if prompts.is_empty() {
            hooks.remove("UserPromptSubmit");
        }
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    count
}

/// Strip the `cc-connect` entry from `mcpServers` in `~/.claude.json`.
/// Writes a `.json.bak` alongside before mutating.
fn remove_mcp_from_claude_json() -> Result<()> {
    let path = home_dir().join(".claude.json");
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("  no ~/.claude.json — nothing to remove");
            return Ok(());
        }
        Err(e) => return Err(anyhow::anyhow!("read {}: {e}", path.display())),
    };
    if raw.trim().is_empty() {
        eprintln!("  ~/.claude.json is empty — nothing to remove");
        return Ok(());
    }

    let mut json: serde_json::Value = serde_json::from_str(&raw).context("parse .claude.json")?;

    let removed = if let Some(servers) = json.get_mut("mcpServers").and_then(|s| s.as_object_mut())
    {
        servers.remove(MCP_SERVER_KEY).is_some()
    } else {
        false
    };

    if removed {
        let backup = path.with_extension(format!("json.bak.{}", now_secs()));
        let _ = std::fs::copy(&path, &backup);
        let written = serde_json::to_string_pretty(&json)? + "\n";
        std::fs::write(&path, written).context("write .claude.json")?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        eprintln!(
            "  removed `{MCP_SERVER_KEY}` MCP server entry (backup: {})",
            backup.display()
        );
    } else {
        eprintln!("  no `{MCP_SERVER_KEY}` MCP server entry found");
    }
    Ok(())
}

/// Best-effort: try `claude mcp remove cc-connect --scope user` so we
/// also clean any user-scope entry the Claude Code CLI tracks separately
/// from the on-disk JSON. Silent if `claude` isn't on PATH.
fn remove_mcp_via_claude_cli() -> Result<()> {
    use std::process::{Command, Stdio};
    if which("claude").is_none() {
        return Ok(());
    }
    let _ = Command::new("claude")
        .args(["mcp", "remove", MCP_SERVER_KEY, "--scope", "user"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Suppress "unused import" warnings when std::path::Path isn't needed; the
// compiler only sees Path used on platforms with this exact set of helpers.
#[allow(dead_code)]
fn _unused_path_marker(_p: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strip_hook_removes_only_cc_connect_entries() {
        let mut v = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "matcher": "",
                        "hooks": [
                            {"type": "command", "command": "/abs/cc-connect-hook"},
                            {"type": "command", "command": "/usr/bin/other-tool"}
                        ]
                    }
                ]
            }
        });
        assert_eq!(strip_cc_connect_hook(&mut v), 1);
        // The other-tool hook entry must survive.
        let arr = v["hooks"]["UserPromptSubmit"][0]["hooks"]
            .as_array()
            .unwrap();
        assert_eq!(arr.len(), 1);
        assert!(arr[0]["command"].as_str().unwrap().contains("other-tool"));
    }

    #[test]
    fn strip_hook_removes_legacy_flat_shape() {
        let mut v = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {"matcher": "", "command": "/abs/cc-connect-hook"}
                ]
            }
        });
        assert_eq!(strip_cc_connect_hook(&mut v), 1);
        // UserPromptSubmit should now be gone (was the only entry).
        assert!(v.get("hooks").is_none() || v["hooks"].get("UserPromptSubmit").is_none());
    }

    #[test]
    fn strip_hook_drops_empty_entry_after_removal() {
        let mut v = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {
                        "matcher": "",
                        "hooks": [{"type": "command", "command": "/abs/cc-connect-hook"}]
                    }
                ]
            }
        });
        assert_eq!(strip_cc_connect_hook(&mut v), 1);
        // No hooks left at all → the whole `hooks` key should be gone.
        assert!(v.get("hooks").is_none());
    }

    #[test]
    fn strip_hook_no_op_when_nothing_matches() {
        let mut v = json!({
            "hooks": {
                "UserPromptSubmit": [
                    {"matcher": "", "hooks": [{"command": "/usr/bin/foo"}]}
                ]
            }
        });
        let before = v.clone();
        assert_eq!(strip_cc_connect_hook(&mut v), 0);
        assert_eq!(v, before);
    }

    #[test]
    fn strip_hook_no_op_on_unrelated_settings() {
        let mut v = json!({"theme": "dark"});
        assert_eq!(strip_cc_connect_hook(&mut v), 0);
        assert_eq!(v, json!({"theme": "dark"}));
    }
}
