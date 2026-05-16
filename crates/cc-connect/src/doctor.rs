//! `cc-connect doctor` — sanity-check the local install.
//!
//! Verifies the on-disk and settings.json conditions cc-connect assumes:
//! - `~/.cc-connect/identity.key` exists with mode 0600 (PROTOCOL.md §2).
//! - `${TMPDIR}/cc-connect-${UID}/active-rooms/` (if present) is mode 0700,
//!   not a symlink (PROTOCOL.md §8 + ADR-0003).
//! - `~/.claude/settings.json` has a `UserPromptSubmit` hook entry whose
//!   command resolves to an executable cc-connect-hook binary
//!   (PROTOCOL.md §7.1).
//!
//! Output is human-readable and structured: each check prints `OK`,
//! `WARN`, or `FAIL`. The function returns `Err` if any FAIL is hit.

use anyhow::{bail, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Run all doctor checks. Returns Err iff any check is FAIL.
pub fn run() -> Result<()> {
    let mut report = Report::default();

    print_self_info(&mut report);
    check_upgrade_target(&mut report);
    check_identity(&mut report);
    check_active_rooms_dir(&mut report);
    check_settings_json_hook(&mut report);
    check_settings_json_mcp(&mut report);

    println!();
    println!(
        "{} ok, {} warn, {} fail",
        report.ok, report.warn, report.fail
    );

    if report.fail > 0 {
        bail!("cc-connect doctor: {} check(s) FAIL", report.fail);
    }
    Ok(())
}

/// Surface the running cc-connect binary's path and build age. The most
/// common "MCP doesn't have cc_wait_for_mention / hook orientation header
/// looks wrong" failure mode is the user shipping with one clone but
/// installing from another — printing path + mtime makes the staleness
/// obvious at a glance.
fn print_self_info(report: &mut Report) {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return,
    };
    let age = age_of(&exe).unwrap_or_else(|| "?".to_string());
    report.info(&format!(
        "cc-connect binary: {} (built {})",
        exe.display(),
        age
    ));
}

/// Surface whether `cc-connect upgrade` will be able to find a git
/// checkout to pull. The historical bug: `~/.local/bin/cc-connect` is a
/// symlink into the source tree, and `cc-connect upgrade` walked up
/// from `~/.local/bin/` rather than the resolved target — so upgrade
/// failed silently for every install built from source. Surfacing
/// "upgrade target" here lets the user see at a glance whether
/// `cc-connect upgrade` will work, or whether they're on a binary
/// install (bootstrap.sh) where re-running bootstrap.sh is the way.
fn check_upgrade_target(report: &mut Report) {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return,
    };
    match crate::lifecycle::locate_install_repo_from(&exe) {
        Ok(repo) => {
            report.ok(&format!("cc-connect upgrade target: {}", repo.display()));
        }
        Err(_) => {
            report.info(
                "no git checkout above this binary — `cc-connect upgrade` won't work. \
                 If you installed via bootstrap.sh, re-run it to upgrade; otherwise \
                 `git pull && ./install.sh` from inside the clone.",
            );
        }
    }
}

/// `mtime` of `path` rendered as a coarse "Ns/m/h/d ago" string, or
/// `None` if the file isn't stat-able / the clock disagrees.
fn age_of(path: &Path) -> Option<String> {
    let mtime = fs::metadata(path).ok()?.modified().ok()?;
    let dur = SystemTime::now().duration_since(mtime).ok()?;
    let secs = dur.as_secs();
    Some(if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{}m ago", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h ago", secs / 86_400, (secs % 86_400) / 3600)
    })
}

/// Returns the number of whole seconds by which `path`'s mtime is older
/// than `reference`'s mtime. Returns `None` if either stat fails or
/// `path` is the same age or newer.
fn staleness_secs(path: &Path, reference: &Path) -> Option<u64> {
    let path_mtime = fs::metadata(path).ok()?.modified().ok()?;
    let ref_mtime = fs::metadata(reference).ok()?.modified().ok()?;
    ref_mtime
        .duration_since(path_mtime)
        .ok()
        .map(|d| d.as_secs())
}

/// Warn if the registered binary at `path` is significantly older than
/// the cc-connect binary currently running. Catches the "I rebuilt in
/// another clone but didn't re-install" footgun.
fn maybe_warn_stale(report: &mut Report, label: &str, path: &Path) {
    let Ok(self_exe) = std::env::current_exe() else {
        return;
    };
    let Some(secs) = staleness_secs(path, &self_exe) else {
        return;
    };
    // 60 s slack to absorb filesystem mtime precision and same-build
    // ordering jitter.
    if secs <= 60 {
        return;
    }
    let pretty = if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600)
    };
    report.warn(&format!(
        "{label} is {pretty} older than the cc-connect binary you ran. \
         If you've rebuilt elsewhere, run `./install.sh` from that clone, \
         then restart Claude Code so it drops the now-stale child."
    ));
}

#[derive(Default)]
struct Report {
    ok: usize,
    warn: usize,
    fail: usize,
}

impl Report {
    fn ok(&mut self, msg: &str) {
        println!("[OK]   {msg}");
        self.ok += 1;
    }
    fn warn(&mut self, msg: &str) {
        println!("[WARN] {msg}");
        self.warn += 1;
    }
    fn fail(&mut self, msg: &str) {
        println!("[FAIL] {msg}");
        self.fail += 1;
    }
    fn info(&self, msg: &str) {
        println!("[--]   {msg}");
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn check_identity(report: &mut Report) {
    let path = home_dir().join(".cc-connect").join("identity.key");
    match fs::metadata(&path) {
        Ok(meta) => {
            let mode = meta.permissions().mode() & 0o777;
            if mode == 0o600 {
                report.ok(&format!("identity.key at {} is mode 0600", path.display()));
            } else {
                report.warn(&format!(
                    "identity.key at {} is mode {:o}, expected 0600 (run `chmod 600 {}`)",
                    path.display(),
                    mode,
                    path.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.info("identity.key not yet created — `cc-connect host` or `chat` will create it on first run");
        }
        Err(e) => {
            report.fail(&format!("could not stat {}: {e}", path.display()));
        }
    }
}

fn check_active_rooms_dir(report: &mut Report) {
    let uid = rustix::process::geteuid().as_raw();
    let dir = std::env::temp_dir()
        .join(format!("cc-connect-{uid}"))
        .join("active-rooms");

    match fs::symlink_metadata(&dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                report.fail(&format!(
                    "{} is a symlink — refuse to operate (potential snoop). \
                     Recover with: rm -rf {}",
                    dir.display(),
                    dir.parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                ));
                return;
            }
            if !meta.is_dir() {
                report.fail(&format!("{} is not a directory", dir.display()));
                return;
            }
            let mode = meta.permissions().mode() & 0o777;
            if mode == 0o700 {
                report.ok(&format!("{} is a directory with mode 0700", dir.display()));
            } else {
                report.fail(&format!(
                    "{} is mode {:o}, expected exactly 0700 (PROTOCOL.md §8); recover with: rm -rf {}",
                    dir.display(),
                    mode,
                    dir.parent().map(|p| p.display().to_string()).unwrap_or_default()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.info(&format!(
                "{} not yet created — first `cc-connect chat` will create it",
                dir.display()
            ));
        }
        Err(e) => {
            report.fail(&format!("could not stat {}: {e}", dir.display()));
        }
    }
}

fn check_settings_json_hook(report: &mut Report) {
    let path = home_dir().join(".claude").join("settings.json");
    let raw = match fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.warn(&format!(
                "{} does not exist — Claude Code is not configured. \
                 To enable cc-connect, add the hook entry from the README.",
                path.display()
            ));
            return;
        }
        Err(e) => {
            report.fail(&format!("could not read {}: {e}", path.display()));
            return;
        }
    };

    let parsed: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            report.fail(&format!("settings.json is not valid JSON: {e}"));
            return;
        }
    };

    let hooks = match parsed.get("hooks").and_then(|h| h.get("UserPromptSubmit")) {
        Some(v) => v,
        None => {
            report.warn("settings.json has no hooks.UserPromptSubmit — cc-connect-hook is not wired into Claude Code");
            return;
        }
    };

    let entries = match hooks.as_array() {
        Some(arr) => arr,
        None => {
            report.fail("settings.json hooks.UserPromptSubmit is not an array");
            return;
        }
    };

    let mut found_command: Option<String> = None;
    for entry in entries {
        // Correct nested shape: {matcher, hooks: [{type, command}, …]}.
        if let Some(hs) = entry.get("hooks").and_then(|x| x.as_array()) {
            for h in hs {
                if let Some(cmd) = h.get("command").and_then(|c| c.as_str()) {
                    if cmd.contains("cc-connect-hook") {
                        found_command = Some(cmd.to_string());
                        break;
                    }
                }
            }
        }
        if found_command.is_some() {
            break;
        }
        // Legacy flat shape that earlier install.sh runs wrote by mistake.
        if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
            if cmd.contains("cc-connect-hook") {
                found_command = Some(cmd.to_string());
                break;
            }
        }
    }
    let cmd = match found_command {
        Some(c) => c,
        None => {
            report.warn(
                "no UserPromptSubmit hook entry mentions cc-connect-hook — install it per README",
            );
            return;
        }
    };

    // The command field may include leading env vars (e.g. `CC_FOO=1 /path/to/bin`).
    // Pull the last token that contains "cc-connect-hook" as the path.
    let bin_path = cmd
        .split_whitespace()
        .rev()
        .find(|tok| tok.contains("cc-connect-hook") && !tok.contains('='))
        .unwrap_or(cmd.as_str());

    let p = Path::new(bin_path);
    if !p.is_absolute() {
        report.warn(&format!(
            "settings.json hook command uses bare/relative path {bin_path}; PROTOCOL.md §7.1 \
             SHOULD be absolute. Claude Code's PATH may not include `{}`.",
            home_dir().join(".cargo/bin").display()
        ));
    }

    match fs::metadata(p) {
        Ok(meta) => {
            let mode = meta.permissions().mode();
            if mode & 0o111 != 0 {
                let age = age_of(p).unwrap_or_else(|| "?".to_string());
                report.ok(&format!(
                    "hook binary {} (built {}) exists and is executable",
                    p.display(),
                    age,
                ));
                maybe_warn_stale(report, "hook binary", p);
            } else {
                report.fail(&format!("{} exists but is not executable", p.display()));
            }
        }
        Err(_) => {
            report.fail(&format!(
                "settings.json points at {} but it does not exist or is not readable",
                p.display()
            ));
        }
    }
}

/// Check that an MCP server pointing at the cc-connect-mcp binary is
/// registered in either of Claude Code's two known config files:
/// - `~/.claude.json::mcpServers` (canonical — what `claude mcp add` writes)
/// - `~/.claude/settings.json::mcpServers` (legacy — older installs)
///
/// Without this, the embedded Claude can't call back into chat (no
/// cc_send / cc_at / cc_drop / etc.).
fn check_settings_json_mcp(report: &mut Report) {
    let canonical = home_dir().join(".claude.json");
    let legacy = home_dir().join(".claude").join("settings.json");

    let mut found: Option<(PathBuf, String)> = None;
    let mut probed_any = false;

    for cfg in [&canonical, &legacy] {
        let raw = match fs::read_to_string(cfg) {
            Ok(r) => r,
            Err(_) => continue,
        };
        probed_any = true;
        let parsed: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(servers) = parsed.get("mcpServers").and_then(|x| x.as_object()) else {
            continue;
        };
        for (_name, entry) in servers {
            if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
                if cmd.contains("cc-connect-mcp") {
                    found = Some((cfg.clone(), cmd.to_string()));
                    break;
                }
            }
        }
        if found.is_some() {
            break;
        }
    }

    if !probed_any {
        report.warn(
            "neither ~/.claude.json nor ~/.claude/settings.json exists — MCP server not registered. \
             Run `./install.sh` (or `cc-connect-tui` once and accept the wizard).",
        );
        return;
    }

    let (cfg_path, cmd) = match found {
        Some(p) => p,
        None => {
            report.warn(
                "no mcpServers entry mentions cc-connect-mcp in ~/.claude.json or ~/.claude/settings.json — \
                 Claude can't call cc_send/cc_at/cc_drop. Run `./install.sh` (or `claude mcp add cc-connect <path>`) to add it.",
            );
            return;
        }
    };

    let p = Path::new(&cmd);
    match fs::metadata(p) {
        Ok(meta) => {
            let mode = meta.permissions().mode();
            if mode & 0o111 != 0 {
                let age = age_of(p).unwrap_or_else(|| "?".to_string());
                report.ok(&format!(
                    "mcp server {} (built {}) exists and is executable (registered in {})",
                    p.display(),
                    age,
                    cfg_path.display()
                ));
                maybe_warn_stale(report, "mcp server", p);
            } else {
                report.fail(&format!("{} exists but is not executable", p.display()));
            }
        }
        Err(_) => {
            report.fail(&format!(
                "{} points at MCP {} but it does not exist",
                cfg_path.display(),
                p.display()
            ));
        }
    }
    report.info(
        "if Claude Code says 'cc-connect MCP not found' even though this check passed, restart Claude Code so it re-reads its config on launch.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_dir_falls_back_to_root() {
        // Just exercises the helper. We can't easily stub HOME in a test,
        // but this ensures the function doesn't panic.
        let h = home_dir();
        assert!(h.is_absolute() || h == Path::new("/"));
    }

    #[test]
    fn report_counts_categories() {
        let mut r = Report::default();
        r.ok("a");
        r.warn("b");
        r.fail("c");
        r.fail("d");
        r.info("e");
        assert_eq!(r.ok, 1);
        assert_eq!(r.warn, 1);
        assert_eq!(r.fail, 2);
    }

    /// Exercises the metadata-vs-symlink distinction without stubbing the
    /// real `/tmp/cc-connect-$UID/active-rooms/`. Just ensures the function
    /// runs without panicking on a clean machine.
    #[test]
    fn check_active_rooms_dir_does_not_panic() {
        let mut r = Report::default();
        check_active_rooms_dir(&mut r);
        // Either OK / WARN / FAIL / INFO — any of those is acceptable here.
        let _ = r.ok + r.warn + r.fail;
    }
}
