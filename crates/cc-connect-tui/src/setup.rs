//! First-run setup wizard.
//!
//! Runs BEFORE the TUI takes over the terminal so the user can answer
//! plain stdin/stdout prompts. Two checks:
//!
//! 1. Hook is wired into `~/.claude/settings.json` (Claude Code's
//!    `UserPromptSubmit` array, in the correct nested `{matcher, hooks:[…]}`
//!    shape). If absent, offer to install it. Idempotent: existing
//!    other-tool entries are preserved; legacy flat entries from earlier
//!    install.sh runs are migrated.
//!
//! 2. (only on `start`) Relay choice — n0 default, custom URL, or skip.
//!    Persisted at `~/.cc-connect/config.json` so we only ask once per
//!    machine. The `--relay <url>` CLI flag overrides the config.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

// ---- Hook (Claude Code settings.json) --------------------------------------

/// If the cc-connect-hook entry is missing from `~/.claude/settings.json`,
/// prompt the user and install on a `y` answer. On any error, print and
/// continue — the TUI will still come up, the hook just won't fire.
pub fn ensure_hook_installed() -> Result<()> {
    let hook_path = locate_hook_binary()?;
    let settings = settings_path();
    if hook_already_installed(&settings, &hook_path)? {
        return Ok(());
    }
    println!();
    println!("cc-connect's UserPromptSubmit hook is not installed in");
    println!("  {}", settings.display());
    println!("Without it, chat lines from your room won't surface in Claude Code.");
    println!();
    if !confirm("Install the hook now?", true)? {
        println!("Skipping. You can install it later via `cc-connect-tui` re-run or `./install.sh`.");
        return Ok(());
    }
    install_hook(&settings, &hook_path)?;
    println!("✓ hook installed (existing settings.json backed up alongside).");
    println!("  Restart Claude Code so it picks up the new hook.");
    Ok(())
}

fn locate_hook_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("current_exe has no parent: {}", exe.display()))?;
    let hook = dir.join("cc-connect-hook");
    if !hook.exists() {
        return Err(anyhow!(
            "expected `cc-connect-hook` next to cc-connect-tui at {}",
            hook.display()
        ));
    }
    Ok(hook)
}

fn settings_path() -> PathBuf {
    home_dir().join(".claude").join("settings.json")
}

fn hook_already_installed(settings: &Path, hook_path: &Path) -> Result<bool> {
    if !settings.exists() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(settings)
        .with_context(|| format!("read {}", settings.display()))?;
    if raw.trim().is_empty() {
        return Ok(false);
    }
    let v: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("parse JSON in {}", settings.display()))?;
    let arr = match v.pointer("/hooks/UserPromptSubmit").and_then(|x| x.as_array()) {
        Some(a) => a,
        None => return Ok(false),
    };
    let target = hook_path.to_string_lossy().to_string();
    for entry in arr {
        // Correct nested shape: {matcher, hooks: [{type, command}, …]}.
        if let Some(hs) = entry.get("hooks").and_then(|x| x.as_array()) {
            for h in hs {
                if h.get("command").and_then(|x| x.as_str()) == Some(&target) {
                    return Ok(true);
                }
            }
        }
        // Legacy flat shape — install.sh used to write this.
        if entry.get("command").and_then(|x| x.as_str()) == Some(&target) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn install_hook(settings: &Path, hook_path: &Path) -> Result<()> {
    let parent = settings
        .parent()
        .ok_or_else(|| anyhow!("settings path has no parent"))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create_dir_all {}", parent.display()))?;
    if settings.exists() {
        let bak = settings.with_extension(format!("json.bak.{}", now_secs()));
        std::fs::copy(settings, &bak)
            .with_context(|| format!("backup {}", settings.display()))?;
    }
    let mut data: serde_json::Value = if settings.exists() {
        let raw = std::fs::read_to_string(settings)?;
        if raw.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
        }
    } else {
        serde_json::json!({})
    };
    let target = hook_path.to_string_lossy().to_string();

    // Walk to .hooks.UserPromptSubmit, creating along the way.
    let hooks = data
        .as_object_mut()
        .ok_or_else(|| anyhow!("settings.json root is not an object"))?
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| anyhow!("settings.hooks is not an object"))?;
    let ups = hooks
        .entry("UserPromptSubmit".to_string())
        .or_insert_with(|| serde_json::json!([]));
    let ups = ups
        .as_array_mut()
        .ok_or_else(|| anyhow!("hooks.UserPromptSubmit is not an array"))?;

    // Drop any existing entry pointing at our hook (in either shape).
    ups.retain(|entry| {
        if entry.get("command").and_then(|x| x.as_str()) == Some(target.as_str()) {
            return false;
        }
        if let Some(hs) = entry.get("hooks").and_then(|x| x.as_array()) {
            for h in hs {
                if h.get("command").and_then(|x| x.as_str()) == Some(target.as_str()) {
                    return false;
                }
            }
        }
        true
    });

    ups.push(serde_json::json!({
        "matcher": "",
        "hooks": [{"type": "command", "command": target}],
    }));

    let written = serde_json::to_string_pretty(&data)? + "\n";
    std::fs::write(settings, &written)
        .with_context(|| format!("write {}", settings.display()))?;
    let _ = std::fs::set_permissions(settings, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

// ---- Self nickname --------------------------------------------------------

/// Read `self_nick` from `~/.cc-connect/config.json`. If absent, prompt
/// once and persist. Empty answer = the user wants no nick (we save an
/// empty string so we don't ask again).
pub fn ensure_self_nick() -> Result<Option<String>> {
    let cfg_path = config_path();
    let mut cfg = read_config(&cfg_path).unwrap_or_default();
    if cfg.self_nick.is_some() {
        return Ok(cfg.self_nick.filter(|s| !s.is_empty()));
    }
    println!();
    println!("Pick a display name (other peers see this as your sender label).");
    println!("Leave blank to use a short pubkey prefix.");
    let raw = match read_line("Display name: ") {
        Ok(s) => s,
        Err(_) => String::new(),
    };
    let nick = raw.trim().to_string();
    if nick.len() > cc_connect_core::message::NICK_MAX_BYTES {
        bail!(
            "nickname too long ({} > {} bytes); shorten and re-run",
            nick.len(),
            cc_connect_core::message::NICK_MAX_BYTES
        );
    }
    if nick.chars().any(|c| c.is_control()) {
        bail!("nickname must not contain control characters");
    }
    cfg.self_nick = Some(nick.clone());
    write_config(&cfg_path, &cfg)?;
    Ok(if nick.is_empty() { None } else { Some(nick) })
}

// ---- Relay config (start mode only) ----------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConnectConfig {
    /// "n0" | "custom" | "skip"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay_mode: Option<String>,
    /// Set when relay_mode = "custom".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay_url: Option<String>,
    /// User's self-declared display name. Picked up by chat_session and
    /// emitted as the v0.2 `nick` field on outgoing Messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    self_nick: Option<String>,
}

const CONFIG_FILENAME: &str = "config.json";

/// Resolve the relay URL for `start`. Precedence:
///  1. Explicit `--relay <url>` flag from the caller (returned as-is).
///  2. Persisted choice in `~/.cc-connect/config.json`.
///  3. First-run prompt; the answer is persisted.
///
/// Returns `None` for "use n0 default" (no URL override needed).
pub fn ensure_relay_choice(provided: Option<&str>) -> Result<Option<String>> {
    if let Some(url) = provided {
        return Ok(Some(url.to_string()));
    }
    let cfg_path = config_path();
    let cfg = read_config(&cfg_path).unwrap_or_default();
    match cfg.relay_mode.as_deref() {
        Some("n0") | Some("skip") => return Ok(None),
        Some("custom") => {
            if let Some(url) = cfg.relay_url.clone() {
                return Ok(Some(url));
            }
        }
        _ => {}
    }
    let answer = prompt_relay_choice()?;
    // Preserve any existing self_nick when we rewrite the config.
    let preserved_nick = cfg.self_nick.clone();
    let (relay_mode, relay_url, returned) = match answer {
        RelayChoice::N0 => (Some("n0".to_string()), None, None),
        RelayChoice::Custom(url) => (
            Some("custom".to_string()),
            Some(url.clone()),
            Some(url),
        ),
        RelayChoice::Skip => (Some("skip".to_string()), None, None),
    };
    let new_cfg = ConnectConfig {
        relay_mode,
        relay_url,
        self_nick: preserved_nick,
    };
    write_config(&cfg_path, &new_cfg)?;
    Ok(returned)
}

enum RelayChoice {
    N0,
    Custom(String),
    Skip,
}

fn prompt_relay_choice() -> Result<RelayChoice> {
    println!();
    println!("Pick a relay (used to traverse NATs / cross networks):");
    println!("  1) n0's free public relay  (default — works everywhere)");
    println!("  2) Self-hosted iroh-relay  (your own server, more privacy)");
    println!("  3) Skip / decide later");
    println!();
    loop {
        let raw = read_line("[1/2/3, default 1]: ")?;
        let trimmed = raw.trim();
        match trimmed {
            "" | "1" | "n0" => return Ok(RelayChoice::N0),
            "2" | "self" | "custom" => {
                let url = read_line("Enter relay URL (e.g. https://relay.you.com): ")?;
                let url = url.trim().to_string();
                if url.is_empty() {
                    println!("(empty URL, treating as skip)");
                    return Ok(RelayChoice::Skip);
                }
                return Ok(RelayChoice::Custom(url));
            }
            "3" | "skip" => return Ok(RelayChoice::Skip),
            other => println!("(didn't understand {other:?}, try 1, 2, or 3)"),
        }
    }
}

fn config_path() -> PathBuf {
    home_dir().join(".cc-connect").join(CONFIG_FILENAME)
}

fn read_config(path: &Path) -> Result<ConnectConfig> {
    if !path.exists() {
        return Ok(ConnectConfig::default());
    }
    let raw = std::fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(ConnectConfig::default());
    }
    Ok(serde_json::from_str(&raw)?)
}

fn write_config(path: &Path, cfg: &ConnectConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let raw = serde_json::to_string_pretty(cfg)? + "\n";
    std::fs::write(path, raw)?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

// ---- prompt helpers --------------------------------------------------------

fn confirm(prompt: &str, default_yes: bool) -> Result<bool> {
    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    let raw = read_line(&format!("{prompt} {suffix} "))?;
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return Ok(default_yes);
    }
    Ok(matches!(trimmed.as_str(), "y" | "yes"))
}

fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut buf = String::new();
    std::io::stdin().lock().read_line(&mut buf).context("read stdin")?;
    if buf.is_empty() {
        bail!("EOF on stdin");
    }
    Ok(buf)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn now_secs() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
