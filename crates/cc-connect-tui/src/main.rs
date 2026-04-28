//! `cc-connect-tui` — vertical-split TUI binary.
//!
//! Two modes:
//!   - `start [--relay <url>]`  — spawn a `cc-connect host-bg` daemon,
//!     join its room, drop you into the TUI.
//!   - `join <ticket> [--relay <url>]` — join an existing room.
//!
//! The `cc-connect room` subcommand in the main `cc-connect` binary just
//! `exec`s into this binary so users can type either form.

use anyhow::{anyhow, Context, Result};
use cc_connect::ticket_payload::TicketPayload;
use cc_connect_core::ticket::decode_room_code;
use cc_connect_tui::RunOpts;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Parser)]
#[command(name = "cc-connect-tui", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start a new room (spawns `cc-connect host-bg` in the background) and
    /// open the TUI.
    Start {
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
    },
    /// Join an existing room by ticket.
    Join {
        ticket: String,
        #[arg(long, value_name = "URL")]
        relay: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Bail early on non-TTY so we don't spin up host-bg, hold an iroh
    // endpoint, etc. before realising we have nowhere to render.
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        return Err(anyhow!(
            "TTY required — `cc-connect-tui` must run in an interactive terminal"
        ));
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;

    rt.block_on(async move {
        match cli.cmd {
            Cmd::Start { relay } => start(relay.as_deref()).await,
            Cmd::Join { ticket, relay } => join(&ticket, relay.as_deref()).await,
        }
    })
}

async fn start(relay: Option<&str>) -> Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    let cc_connect = exe
        .parent()
        .and_then(|p| Some(p.join("cc-connect")))
        .ok_or_else(|| anyhow!("can't locate `cc-connect` binary next to cc-connect-tui"))?;
    if !cc_connect.exists() {
        return Err(anyhow!(
            "expected `cc-connect` at {} — both binaries must live side-by-side",
            cc_connect.display()
        ));
    }

    let mut cmd = Command::new(&cc_connect);
    cmd.arg("host-bg").arg("start");
    if let Some(r) = relay {
        cmd.arg("--relay").arg(r);
    }
    let out = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("spawn {}", cc_connect.display()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!(
            "host-bg start failed (exit {:?}): {stderr}",
            out.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let ticket = stdout
        .lines()
        .find_map(|l| {
            l.split_whitespace()
                .find(|w| w.starts_with("cc1-"))
                .map(|s| s.to_string())
        })
        .ok_or_else(|| anyhow!("host-bg start did not print a ticket; output was:\n{stdout}"))?;
    println!("[room] daemon started, joining…");

    enter_tui(ticket, relay).await
}

async fn join(ticket: &str, relay: Option<&str>) -> Result<()> {
    enter_tui(ticket.to_string(), relay).await
}

async fn enter_tui(ticket: String, relay: Option<&str>) -> Result<()> {
    let topic_hex = topic_hex_from_ticket(&ticket)?;
    let claude_argv = std::env::var("CC_CONNECT_CLAUDE_BIN")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| vec![s])
        .unwrap_or_else(|| vec!["claude".to_string()]);
    let claude_cwd: Option<PathBuf> = std::env::current_dir().ok();

    let opts = RunOpts {
        ticket,
        topic_hex,
        no_relay: false,
        relay: relay.map(|s| s.to_string()),
        claude_argv,
        claude_cwd,
    };
    cc_connect_tui::run(opts).await
}

fn topic_hex_from_ticket(ticket: &str) -> Result<String> {
    let bytes = decode_room_code(ticket).with_context(|| format!("decode ticket: {ticket:.20}…"))?;
    let payload = TicketPayload::from_bytes(&bytes).context("parse ticket payload")?;
    let mut hex = String::with_capacity(64);
    for b in payload.topic.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    Ok(hex)
}

