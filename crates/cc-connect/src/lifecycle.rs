//! `cc-connect clear` and `cc-connect uninstall` — the wind-down commands.
//!
//! `clear` stops all running cc-connect background processes
//! (chat-daemons + host-bg daemons). The `--purge` flag also wipes
//! `~/.cc-connect/rooms/` AND the per-UID tmp dir (`/tmp/cc-connect-$UID/`)
//! so the next room start truly starts from scratch (e.g. when a stuck
//! daemon left a corrupted log.jsonl, an orphan PID file, or an unbound
//! IPC socket behind).
//!
//! `uninstall` reverses what `install.sh` did:
//!   1. `clear` — stop everything
//!   2. strip the cc-connect-hook entry from `~/.claude/settings.json`
//!   3. strip the cc-connect MCP server entry from `~/.claude.json`
//!   4. remove the `~/.local/bin/cc-connect{,-hook,-mcp,-tui}` and
//!      `cc-chat-ui` symlinks
//!
//! With `--purge` it also removes:
//!   - `~/.cc-connect/` (identity, nicknames, every room)
//!   - `/tmp/cc-connect-$UID/` (active-rooms PID files, IPC sockets)
//!   - `~/.claude.json.bak.*` and `~/.claude/*.json.bak.*`
//!     (the timestamped JSON backups install.sh / setup.rs / lifecycle.rs
//!     write — they accumulate forever otherwise)
//!
//! Both commands are best-effort: they log every step and continue past
//! per-step failures so a half-broken install can still be cleaned up.

use anyhow::{Context, Result};
use cc_connect_core::posix::cc_connect_uid_dir;
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

    // GC orphaned `~/.cc-connect/sessions/by-claude-pid/<pid>/` dirs whose
    // owning Claude exited without explicit `cc_leave_room`. Same prune
    // the hook + MCP server run on startup; running it from `clear` keeps
    // `cc-connect pending-list` and `cc-connect-mcp::cc_list_rooms`
    // honest after a Claude crash. ADR-0006.
    match cc_connect_core::session_state::prune_dead_pid_sessions() {
        Ok(0) => {}
        Ok(n) => eprintln!("[clear] pruned {n} dead-PID session entries"),
        Err(e) => errors.push(format!("prune dead PID sessions: {e:#}")),
    }

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
        // Wipe the MCP-first per-Claude state. Equivalent to "every Claude
        // window starts fresh after this command." Plain `clear` (no
        // --purge) only prunes DEAD entries above; --purge wipes
        // everything including live sessions, so currently-running
        // Claudes lose their bound rooms — they'd need to re-call
        // cc_create_room / cc_join_room.
        let sessions_dir = home_dir().join(".cc-connect").join("sessions");
        if sessions_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&sessions_dir) {
                eprintln!("  warn: rm -rf {}: {e}", sessions_dir.display());
            } else {
                eprintln!("[clear] purged {}", sessions_dir.display());
            }
        }
        let pending_dir = home_dir().join(".cc-connect").join("pending-joins");
        if pending_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&pending_dir) {
                eprintln!("  warn: rm -rf {}: {e}", pending_dir.display());
            } else {
                eprintln!("[clear] purged {}", pending_dir.display());
            }
        }
        // Sweep the per-UID tmp tree in case a daemon was SIGKILL'd
        // and skipped its Drop-guard cleanup. See `purge_tmp_uid_dir`.
        purge_tmp_uid_dir();
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
        purge_tmp_uid_dir();
        purge_claude_backup_files(&home_dir());
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

// ---- `cc-connect upgrade` --------------------------------------------------

/// Pull the latest source from the install repo's `origin`, run a clean
/// uninstall (without `--purge`, so identity + nicknames survive), then
/// re-run `install.sh` from the freshly-pulled repo. End state: every
/// cc-connect binary, hook entry, MCP entry, and `~/.local/bin` symlink
/// points at code from the new HEAD; user identity + room state survive.
///
/// `yes`: if true, skip the interactive confirmation. Used by CI / scripted
/// upgrades. Without it, the user is prompted after the diff preview.
///
/// Implementation contract: at no point is the running process killed.
/// `cc-connect upgrade` exec's `install.sh` at the end, so the new
/// install fully replaces the old before the user gets their shell back.
/// Identity + nicknames (`~/.cc-connect/identity.key`, `nicknames.json`)
/// are NEVER touched here — that's `cc-connect uninstall --purge`'s job.
pub fn run_upgrade(yes: bool) -> Result<()> {
    use std::io::Write;

    eprintln!("[upgrade] cc-connect");

    // 1. Find the install repo (walk up from current_exe until .git).
    let repo = locate_install_repo().context("locate cc-connect install repo")?;
    eprintln!("[upgrade] install repo: {}", repo.display());

    // 2. Fetch latest from origin.
    eprintln!("[upgrade] fetching origin...");
    if !run_git(&repo, &["fetch", "origin"]) {
        anyhow::bail!("git fetch origin failed in {}", repo.display());
    }

    // 3. Determine current branch + compare HEAD to origin/<branch>.
    let branch = git_current_branch(&repo).context("read current branch")?;
    let local = git_rev_parse(&repo, "HEAD").context("rev-parse HEAD")?;
    let remote_ref = format!("origin/{branch}");
    let remote =
        git_rev_parse(&repo, &remote_ref).with_context(|| format!("rev-parse {remote_ref}"))?;

    if local == remote {
        eprintln!(
            "[upgrade] already at the latest commit on {branch} ({}). Nothing to do.",
            &local[..8.min(local.len())]
        );
        return Ok(());
    }

    let ahead = git_count(&repo, &format!("{local}..{remote}"));
    let behind = git_count(&repo, &format!("{remote}..{local}"));
    eprintln!(
        "[upgrade] {behind} local commit(s) ahead of {remote_ref}, {ahead} remote commit(s) ahead of HEAD."
    );
    if behind > 0 {
        eprintln!(
            "[upgrade] WARNING: local HEAD has commits not in {remote_ref}. \
             A fast-forward pull will fail. Resolve manually before retrying upgrade."
        );
        anyhow::bail!("local branch has diverged");
    }

    eprintln!("[upgrade] incoming commits:");
    let _ = run_git(
        &repo,
        &[
            "log",
            "--oneline",
            "--no-decorate",
            &format!("{local}..{remote}"),
        ],
    );

    // 4. Confirm.
    if !yes {
        eprintln!();
        if !confirm_yn("Proceed with upgrade?", true)? {
            eprintln!("[upgrade] cancelled.");
            return Ok(());
        }
    }

    // 5. Stop daemons + strip stale config (uninstall without --purge).
    eprintln!();
    eprintln!("[upgrade] running uninstall (no --purge — identity + nicknames preserved)...");
    if let Err(e) = run_uninstall(false) {
        eprintln!("[upgrade] warn: uninstall partial: {e:#}");
    }

    // 6. git pull --ff-only.
    eprintln!();
    eprintln!("[upgrade] pulling latest source...");
    if !run_git(&repo, &["pull", "--ff-only", "origin", &branch]) {
        anyhow::bail!(
            "git pull --ff-only failed. Resolve manually in {} and re-run.",
            repo.display()
        );
    }

    // 7. Re-run install.sh from the freshly-pulled repo. install.sh
    // rebuilds, re-symlinks ~/.local/bin/, and re-registers the hook +
    // MCP entries against the new binaries. We pass --yes if the
    // caller asked us to skip prompts.
    let install_sh = repo.join("install.sh");
    if !install_sh.exists() {
        anyhow::bail!(
            "install.sh not found at {} — repo layout changed?",
            install_sh.display()
        );
    }

    eprintln!();
    eprintln!("[upgrade] running install.sh...");
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&install_sh).current_dir(&repo);
    if yes {
        cmd.arg("--yes");
    }
    let status = cmd
        .status()
        .with_context(|| format!("spawn {}", install_sh.display()))?;
    if !status.success() {
        anyhow::bail!("install.sh exited with {:?}", status.code());
    }

    let _ = std::io::stdout().flush();
    eprintln!();
    eprintln!("[upgrade] done.");
    eprintln!("  • Restart Claude Code so it spawns the new MCP server child.");
    eprintln!(
        "  • New binaries are in {}/target/release/.",
        repo.display()
    );
    Ok(())
}

/// Walk up from the running cc-connect binary's parent looking for a
/// `.git` directory. Errors if none found within 6 levels.
fn locate_install_repo() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    let mut dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current_exe has no parent: {}", exe.display()))?
        .to_path_buf();
    for _ in 0..6 {
        if dir.join(".git").exists() {
            return Ok(dir);
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    anyhow::bail!(
        "could not locate cc-connect install repo above {} — is the binary running from a non-git location? \
         Re-run upgrade from inside the cc-connect clone.",
        exe.display()
    );
}

fn run_git(repo: &Path, args: &[&str]) -> bool {
    use std::process::{Command, Stdio};
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git_rev_parse(repo: &Path, refname: &str) -> Result<String> {
    use std::process::{Command, Stdio};
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", refname])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("spawn git rev-parse {refname}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "git rev-parse {refname} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_current_branch(repo: &Path) -> Result<String> {
    use std::process::{Command, Stdio};
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["symbolic-ref", "--short", "HEAD"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("spawn git symbolic-ref")?;
    if !out.status.success() {
        anyhow::bail!(
            "git symbolic-ref --short HEAD failed (detached HEAD?): {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_count(repo: &Path, range: &str) -> usize {
    use std::process::{Command, Stdio};
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-list", "--count", range])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok();
    out.and_then(|o| {
        String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse::<usize>()
            .ok()
    })
    .unwrap_or(0)
}

fn confirm_yn(prompt: &str, default_yes: bool) -> Result<bool> {
    use std::io::Write;
    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    print!("{prompt} {suffix} ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("read stdin")?;
    let trimmed = input.trim().to_lowercase();
    Ok(if trimmed.is_empty() {
        default_yes
    } else {
        matches!(trimmed.as_str(), "y" | "yes")
    })
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

/// Recursively wipe `$TMPDIR/cc-connect-<uid>/` AND `/tmp/cc-connect-<uid>/`
/// (macOS distinguishes the two; on Linux they collapse). Best-effort —
/// missing dirs are no-ops. Sweeps debris from SIGKILL'd daemons whose
/// Drop guards never ran.
fn purge_tmp_uid_dir() {
    let uid = rustix::process::geteuid().as_raw();
    let mut candidates = vec![
        cc_connect_uid_dir(),
        PathBuf::from(format!("/tmp/cc-connect-{uid}")),
    ];
    candidates.sort();
    candidates.dedup();
    for path in candidates {
        match std::fs::remove_dir_all(&path) {
            Ok(()) => eprintln!("[uninstall] purged {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("  warn: rm -rf {}: {e}", path.display()),
        }
    }
}

/// Whether `name` looks like a cc-connect-issued timestamped JSON backup —
/// i.e. install.sh / setup.rs / lifecycle.rs's `<basename>.json.bak.<digits>`
/// convention, or the bare-suffix `<basename>.json.bak` legacy form. The
/// digit suffix matters: third-party tools sometimes write
/// `myproject.json.bak.tag` and we must not touch those.
fn is_cc_connect_backup(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix(".claude.json.bak.") {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    if let Some(idx) = name.rfind(".json.bak.") {
        let suffix = &name[idx + ".json.bak.".len()..];
        return !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit());
    }
    name.ends_with(".json.bak")
}

/// Sweep `<home>/.claude.json.bak.<ts>` and `<home>/.claude/*.json.bak.<ts>`.
/// `home` is passed in so tests can run against a temp dir without
/// mutating the process-global `HOME` env var (cargo runs unit tests
/// concurrently — set_var would race other tests).
fn purge_claude_backup_files(home: &Path) {
    let mut removed = 0usize;
    let dirs = [home.to_path_buf(), home.join(".claude")];
    for dir in &dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(s) = name.to_str() else { continue };
            if !is_cc_connect_backup(s) {
                continue;
            }
            if let Err(e) = std::fs::remove_file(entry.path()) {
                eprintln!("  warn: rm {}: {e}", entry.path().display());
            } else {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        eprintln!("[uninstall] purged {removed} stale .json.bak file(s)");
    }
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

    /// `purge_claude_backup_files` MUST find every install.sh / setup.rs /
    /// lifecycle.rs-issued `<basename>.json.bak.<ts>` (plus the legacy
    /// bare-suffix form), MUST leave the live config files alone, and
    /// MUST NOT delete third-party files that happen to share the
    /// `.json.bak.` substring without the trailing digit timestamp
    /// (e.g. another tool's `myapp.json.bak.snapshot1`).
    #[test]
    fn purge_claude_backups_removes_only_dated_cc_connect_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        let top1 = home.join(".claude.json.bak.1700000000");
        let top2 = home.join(".claude.json.bak.1700099999");
        std::fs::write(&top1, b"{}").unwrap();
        std::fs::write(&top2, b"{}").unwrap();
        let claude = home.join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let s1 = claude.join("settings.json.bak.1700000001");
        let s2 = claude.join("settings.json.bak");
        std::fs::write(&s1, b"{}").unwrap();
        std::fs::write(&s2, b"{}").unwrap();

        let keep1 = home.join(".claude.json");
        let keep2 = claude.join("settings.json");
        let keep3 = claude.join("notes.txt");
        let keep4 = claude.join("myproject.json.bak.snapshot");
        std::fs::write(&keep1, b"{}").unwrap();
        std::fs::write(&keep2, b"{}").unwrap();
        std::fs::write(&keep3, b"hi").unwrap();
        std::fs::write(&keep4, b"{}").unwrap();

        purge_claude_backup_files(home);

        for gone in [&top1, &top2, &s1, &s2] {
            assert!(!gone.exists(), "expected {} to be purged", gone.display());
        }
        for kept in [&keep1, &keep2, &keep3, &keep4] {
            assert!(kept.exists(), "MUST NOT have removed {}", kept.display());
        }
    }

    /// `purge_tmp_uid_dir` is best-effort: it must tolerate a missing
    /// directory tree (the typical post-uninstall state) without error,
    /// and on a present tree it MUST recursively remove it.
    #[test]
    fn purge_tmp_uid_dir_handles_missing_and_present() {
        // Missing case: nothing on disk → no panic, no error printed
        // beyond the existing "no rooms directory" path. Just assert
        // it doesn't unwind.
        purge_tmp_uid_dir();

        // Present case: pre-create the per-UID tree under both
        // candidate roots `purge_tmp_uid_dir` sweeps and verify both
        // get unlinked.
        let uid = rustix::process::geteuid().as_raw();
        let candidates = [
            std::env::temp_dir().join(format!("cc-connect-{uid}")),
            std::path::PathBuf::from(format!("/tmp/cc-connect-{uid}")),
        ];
        for path in &candidates {
            std::fs::create_dir_all(path.join("active-rooms")).ok();
            std::fs::write(path.join("active-rooms").join("test.active"), b"99999").ok();
        }
        purge_tmp_uid_dir();
        for path in &candidates {
            assert!(!path.exists(), "expected {} to be purged", path.display());
        }
    }
}
