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

/// Run all doctor checks. Returns Err iff any check is FAIL.
pub fn run() -> Result<()> {
    let mut report = Report::default();

    check_identity(&mut report);
    check_active_rooms_dir(&mut report);
    check_settings_json_hook(&mut report);

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
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
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
            report.info(&format!(
                "identity.key not yet created — `cc-connect host` or `chat` will create it on first run"
            ));
        }
        Err(e) => {
            report.fail(&format!(
                "could not stat {}: {e}",
                path.display()
            ));
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
                    dir.parent().map(|p| p.display().to_string()).unwrap_or_default()
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
            report.warn(&format!(
                "settings.json has no hooks.UserPromptSubmit — cc-connect-hook is not wired into Claude Code"
            ));
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
            report.warn(&format!(
                "no UserPromptSubmit hook entry mentions cc-connect-hook — install it per README"
            ));
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
                report.ok(&format!("hook binary {} exists and is executable", p.display()));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_dir_falls_back_to_root() {
        // Just exercises the helper. We can't easily stub HOME in a test,
        // but this ensures the function doesn't panic.
        let h = home_dir();
        assert!(h.is_absolute() || h == PathBuf::from("/"));
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
