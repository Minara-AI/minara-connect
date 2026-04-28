//! `cc-connect room {start,join}` — thin shim over the `cc-connect-tui`
//! binary that ships next to ours. We use exec-into-binary rather than a
//! library dep to avoid a Cargo cycle (cc-connect-tui already depends on
//! cc-connect for chat_session).

use anyhow::{anyhow, Context, Result};
use std::os::unix::process::CommandExt;
use std::process::Command;

pub fn run_start(relay: Option<&str>) -> Result<()> {
    let tui = locate_tui_bin()?;
    let mut cmd = Command::new(&tui);
    cmd.arg("start");
    if let Some(r) = relay {
        cmd.arg("--relay").arg(r);
    }
    // exec replaces this process — no nesting, the TUI owns the terminal.
    let err = cmd.exec();
    Err(anyhow!("exec {} failed: {err}", tui.display()))
}

pub fn run_join(ticket: &str, relay: Option<&str>) -> Result<()> {
    let tui = locate_tui_bin()?;
    let mut cmd = Command::new(&tui);
    cmd.arg("join").arg(ticket);
    if let Some(r) = relay {
        cmd.arg("--relay").arg(r);
    }
    let err = cmd.exec();
    Err(anyhow!("exec {} failed: {err}", tui.display()))
}

fn locate_tui_bin() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow!("current_exe has no parent: {}", exe.display()))?;
    let tui = dir.join("cc-connect-tui");
    if !tui.exists() {
        return Err(anyhow!(
            "expected `cc-connect-tui` at {} — is it built? Try: cargo build --workspace --release",
            tui.display()
        ));
    }
    Ok(tui)
}
