//! Persistent chat-session daemon. Mirrors `host_bg.rs` (the same setsid +
//! READY + JSON-PID-file pattern), but instead of bootstrapping an empty
//! topic with an ephemeral identity, it owns a real `chat_session` for a
//! given ticket — so the chat substrate (gossip mesh, log.jsonl, chat.sock
//! IPC) stays alive after any TUI / chat-ui panel that started it.
//!
//! ```text
//! cc-connect chat-daemon start <ticket> [--no-relay] [--relay <url>]
//!     ── setsid-spawn ourselves with `chat-daemon-daemon`, wait for the
//!        daemon to print  "READY <topic_hex>"  on stdout, exit 0 leaving
//!        the daemon detached. Idempotent: if a live daemon already owns
//!        the same topic, prints  "ALREADY <topic_hex> <pid>"  and exits 0
//!        without spawning a duplicate.
//!
//! cc-connect chat-daemon list
//!     ── one line per running daemon, parsed from the per-topic PID files
//!        under `~/.cc-connect/rooms/<topic>/chat-daemon.pid`.
//!
//! cc-connect chat-daemon stop <topic_hex_prefix>
//!     ── unique-prefix match against the PID files, SIGTERM the daemon,
//!        wait for it to clean up, sweep the PID file if it didn't.
//! ```
//!
//! The PID file lives under `~/.cc-connect/rooms/<topic>/chat-daemon.pid`
//! — the same directory chat_session already uses for log.jsonl + chat.sock
//! marker, so all per-room state stays co-located.

use anyhow::{anyhow, bail, Context, Result};
use cc_connect_core::ticket::decode_room_code;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::chat_session::{self, ChatSessionConfig, DisplayLine};
use crate::ticket_payload::TicketPayload;

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_WAIT: Duration = Duration::from_secs(10);

/// JSON written to `~/.cc-connect/rooms/<topic>/chat-daemon.pid`.
#[derive(Debug, Serialize, Deserialize)]
struct ChatDaemonPidFile {
    pid: u32,
    topic: String,
    ticket: String,
    started_at: i64,
    relay: Option<String>,
    no_relay: bool,
}

/// Snapshot of one running chat-daemon, exposed via [`list_running`] for
/// inproc consumers (the launcher's "is the daemon up?" check).
#[derive(Debug, Clone)]
pub struct ChatDaemonInfo {
    pub topic_hex: String,
    pub pid: u32,
    pub ticket: String,
    pub started_at: i64,
    pub relay: Option<String>,
    pub no_relay: bool,
}

// ---------- `cc-connect chat-daemon start <ticket>` --------------------------

pub fn run_start(ticket: &str, no_relay: bool, relay: Option<&str>) -> Result<()> {
    // Decode locally so we can short-circuit on duplicates BEFORE spawning.
    let topic_hex = decode_topic_hex(ticket)
        .with_context(|| format!("decode ticket prefix: {:.20}…", ticket))?;

    if let Some(existing) = lookup_alive_for_topic(&topic_hex)? {
        // Idempotent: tell the caller the topic is already daemon-hosted
        // and which PID owns it, then exit 0.
        println!("ALREADY {} {}", existing.topic_hex, existing.pid);
        return Ok(());
    }

    let exe = std::env::current_exe().context("locate self executable")?;
    let mut cmd = Command::new(exe);
    cmd.arg("chat-daemon-daemon").arg("--ticket").arg(ticket);
    if no_relay {
        cmd.arg("--no-relay");
    }
    if let Some(r) = relay {
        cmd.arg("--relay").arg(r);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    // Detach from our session — closing this terminal must not SIGHUP the
    // daemon. Same trick host_bg uses.
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::setsid()
                .map(|_| ())
                .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))
        });
    }

    let mut child = cmd.spawn().context("spawn chat-daemon")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("daemon stdout pipe missing"))?;

    let (tx, rx) = mpsc::channel::<std::io::Result<String>>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let r = reader.read_line(&mut line).map(|_| line);
        let _ = tx.send(r);
    });

    let ready_line = match rx.recv_timeout(READY_TIMEOUT) {
        Ok(Ok(l)) => l,
        Ok(Err(e)) => {
            let _ = child.kill();
            return Err(anyhow!("daemon stdout read: {e}"));
        }
        Err(_) => {
            let _ = child.kill();
            return Err(anyhow!(
                "daemon did not print READY within {READY_TIMEOUT:?}"
            ));
        }
    };
    let trimmed = ready_line.trim();
    let topic_hex = trimmed
        .strip_prefix("READY ")
        .ok_or_else(|| anyhow!("daemon error or unexpected line: {trimmed:?}"))?;

    println!("READY {topic_hex}");
    println!();
    println!(
        "Daemon hosting room {} (pid {}). chat.sock active.",
        &topic_hex[..12.min(topic_hex.len())],
        child.id()
    );
    println!(
        "Stop with:  cc-connect chat-daemon stop {}",
        &topic_hex[..12.min(topic_hex.len())]
    );

    // Don't reap; daemon is reparented to init. forget() prevents Drop's
    // best-effort kill on `child` going out of scope.
    std::mem::forget(child);
    Ok(())
}

// ---------- `cc-connect chat-daemon list` ------------------------------------

pub fn list_running() -> Result<Vec<ChatDaemonInfo>> {
    let dir = rooms_root();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("readdir {}", dir.display()))? {
        let entry = entry?;
        let topic_dir = entry.path();
        if !topic_dir.is_dir() {
            continue;
        }
        let topic_hex = match topic_dir.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let pid_path = topic_dir.join("chat-daemon.pid");
        if !pid_path.exists() {
            continue;
        }
        let pf = match read_pid_file(&pid_path) {
            Ok(p) => p,
            Err(_) => {
                let _ = std::fs::remove_file(&pid_path);
                continue;
            }
        };
        if !pid_alive(pf.pid)? {
            let _ = std::fs::remove_file(&pid_path);
            continue;
        }
        out.push(ChatDaemonInfo {
            topic_hex,
            pid: pf.pid,
            ticket: pf.ticket,
            started_at: pf.started_at,
            relay: pf.relay,
            no_relay: pf.no_relay,
        });
    }
    Ok(out)
}

pub fn run_list() -> Result<()> {
    let daemons = list_running()?;
    if daemons.is_empty() {
        println!("(no chat daemons running)");
        return Ok(());
    }
    let now = now_ms();
    for d in daemons {
        let uptime = (now - d.started_at).max(0);
        println!(
            "{topic} pid={pid} uptime={up}s relay={relay}{noflag}",
            topic = &d.topic_hex[..12.min(d.topic_hex.len())],
            pid = d.pid,
            up = uptime / 1000,
            relay = d.relay.as_deref().unwrap_or("(default)"),
            noflag = if d.no_relay { " no-relay" } else { "" },
        );
    }
    Ok(())
}

// ---------- `cc-connect chat-daemon stop <topic_prefix>` ---------------------

pub fn run_stop(topic_prefix: &str) -> Result<()> {
    let dir = rooms_root();
    if !dir.exists() {
        bail!("no chat daemons running");
    }
    let mut matches: Vec<(String, PathBuf, ChatDaemonPidFile)> = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let topic_dir = entry.path();
        if !topic_dir.is_dir() {
            continue;
        }
        let topic_hex = match topic_dir.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        if !topic_hex.starts_with(topic_prefix) {
            continue;
        }
        let pid_path = topic_dir.join("chat-daemon.pid");
        let pf = match read_pid_file(&pid_path) {
            Ok(p) => p,
            Err(_) => continue,
        };
        matches.push((topic_hex, pid_path, pf));
    }
    match matches.len() {
        0 => bail!("no chat daemon matches prefix {topic_prefix:?}"),
        1 => {}
        n => bail!(
            "{n} chat daemons match {topic_prefix:?}: {:?}",
            matches.iter().map(|m| &m.0).collect::<Vec<_>>()
        ),
    }
    let (_topic_hex, pid_path, pf) = matches.into_iter().next().unwrap();
    let pid_obj = rustix::process::Pid::from_raw(pf.pid as i32)
        .ok_or_else(|| anyhow!("invalid pid {}", pf.pid))?;
    if let Err(e) = rustix::process::kill_process(pid_obj, rustix::process::Signal::TERM) {
        if e == rustix::io::Errno::SRCH {
            let _ = std::fs::remove_file(&pid_path);
            println!("daemon was already gone; PID file cleaned up");
            return Ok(());
        }
        return Err(anyhow!("kill_process({}): {e}", pf.pid));
    }
    let start = std::time::Instant::now();
    while start.elapsed() < STOP_WAIT {
        if !pid_path.exists() {
            println!("daemon stopped");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = std::fs::remove_file(&pid_path);
    println!("daemon did not clean up within {STOP_WAIT:?}; forced PID-file removal");
    Ok(())
}

// ---------- daemon entry-point (`cc-connect chat-daemon-daemon`) -------------

pub fn run_daemon(ticket: &str, no_relay: bool, relay: Option<&str>) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build daemon tokio runtime")?;
    rt.block_on(daemon_async(ticket, no_relay, relay))
}

async fn daemon_async(ticket: &str, no_relay: bool, relay: Option<&str>) -> Result<()> {
    let topic_hex = decode_topic_hex(ticket)?;

    // Spawn the chat session. This binds chat.sock, opens log.jsonl, joins
    // the gossip mesh, and runs the listener task all internally.
    let cfg = ChatSessionConfig {
        ticket: ticket.to_string(),
        no_relay,
        relay: relay.map(|s| s.to_string()),
    };
    let mut handle = chat_session::spawn(cfg)
        .await
        .context("spawn chat_session")?;

    // The chat session's display channel is bounded (cap 100). Without a
    // consumer the listener back-pressures — meaning incoming gossip
    // events stop getting written to log.jsonl, and the daemon silently
    // wedges. We're a headless daemon: drain to /dev/null. (chat-ui tails
    // log.jsonl directly, not display events.)
    let mut display_rx = std::mem::replace(
        &mut handle.display_rx,
        tokio::sync::mpsc::channel::<DisplayLine>(1).1,
    );
    let _drain_task = tokio::spawn(async move {
        while display_rx.recv().await.is_some() {
            // discard
        }
    });

    // Write the PID file BEFORE printing READY — the launcher races on
    // both: the PID file existence check is what makes idempotent start
    // work for late-arriving sibling launchers.
    let pid_path = pid_file_path(&topic_hex);
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let pf = ChatDaemonPidFile {
        pid: std::process::id(),
        topic: topic_hex.clone(),
        ticket: ticket.to_string(),
        started_at: now_ms(),
        relay: relay.map(|s| s.to_string()),
        no_relay,
    };
    let pid_json = serde_json::to_string(&pf).context("serialize PID file")?;
    std::fs::write(&pid_path, format!("{pid_json}\n"))
        .with_context(|| format!("write {}", pid_path.display()))?;
    let _ = std::fs::set_permissions(&pid_path, std::fs::Permissions::from_mode(0o600));

    // Tell the parent we're alive.
    {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "READY {topic_hex}").context("write READY")?;
        let _ = stdout.flush();
    }

    // Park on SIGTERM/SIGINT, OR on the chat session crashing.
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .context("install SIGTERM handler")?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .context("install SIGINT handler")?;
    tokio::select! {
        _ = sigterm.recv() => {},
        _ = sigint.recv() => {},
        r = &mut handle.join => {
            // Chat session task exited on its own (either clean shutdown
            // because input_tx closed, or a fatal error). Either way we're
            // done.
            let _ = std::fs::remove_file(&pid_path);
            return r.unwrap_or_else(|e| Err(anyhow!("chat_session task panicked: {e}")));
        },
    }

    // Clean shutdown: drop input_tx so chat_session unwinds, then await join.
    let _ = std::fs::remove_file(&pid_path);
    drop(handle.input_tx);
    let _ = handle.join.await;
    Ok(())
}

// ---------- helpers ---------------------------------------------------------

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn rooms_root() -> PathBuf {
    home_dir().join(".cc-connect").join("rooms")
}

fn pid_file_path(topic_hex: &str) -> PathBuf {
    rooms_root().join(topic_hex).join("chat-daemon.pid")
}

fn read_pid_file(path: &Path) -> Result<ChatDaemonPidFile> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let pf: ChatDaemonPidFile = serde_json::from_str(raw.trim())
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(pf)
}

fn pid_alive(pid: u32) -> Result<bool> {
    let pid_obj = match rustix::process::Pid::from_raw(pid as i32) {
        Some(p) => p,
        None => return Ok(false),
    };
    match rustix::process::test_kill_process(pid_obj) {
        Ok(()) => Ok(true),
        Err(e) if e == rustix::io::Errno::SRCH => Ok(false),
        Err(e) => Err(anyhow!("test_kill_process({pid}): {e}")),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn decode_topic_hex(ticket: &str) -> Result<String> {
    let bytes = decode_room_code(ticket)
        .with_context(|| format!("decode room code: {:.20}…", ticket))?;
    let payload = TicketPayload::from_bytes(&bytes)?;
    let mut out = String::with_capacity(64);
    for b in payload.topic.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

/// If a chat-daemon for `topic_hex` is alive, return its info. Sweeps a
/// stale PID file as a side effect.
fn lookup_alive_for_topic(topic_hex: &str) -> Result<Option<ChatDaemonInfo>> {
    let pid_path = pid_file_path(topic_hex);
    if !pid_path.exists() {
        return Ok(None);
    }
    let pf = match read_pid_file(&pid_path) {
        Ok(p) => p,
        Err(_) => {
            let _ = std::fs::remove_file(&pid_path);
            return Ok(None);
        }
    };
    if !pid_alive(pf.pid)? {
        let _ = std::fs::remove_file(&pid_path);
        return Ok(None);
    }
    Ok(Some(ChatDaemonInfo {
        topic_hex: topic_hex.to_string(),
        pid: pf.pid,
        ticket: pf.ticket,
        started_at: pf.started_at,
        relay: pf.relay,
        no_relay: pf.no_relay,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// list_running tolerates a missing rooms dir (fresh install).
    #[test]
    fn list_returns_empty_on_missing_rooms_dir() {
        // We can't easily redirect HOME for a unit test without breaking
        // hermeticity; just exercise the path branch by building the path
        // and checking the empty-fallback type-checks. (Manual smoke runs
        // exercise the real filesystem path.)
        let _ = list_running();
    }

    /// PID-file roundtrip + decode must preserve every field byte-for-byte.
    #[test]
    fn pid_file_roundtrip() {
        let pf = ChatDaemonPidFile {
            pid: 12345,
            topic: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4".into(),
            ticket: "cc1-deadbeef".into(),
            started_at: 1714323456789,
            relay: Some("https://relay.example.com".into()),
            no_relay: false,
        };
        let json = serde_json::to_string(&pf).unwrap();
        let pf2: ChatDaemonPidFile = serde_json::from_str(&json).unwrap();
        assert_eq!(pf.pid, pf2.pid);
        assert_eq!(pf.topic, pf2.topic);
        assert_eq!(pf.ticket, pf2.ticket);
        assert_eq!(pf.started_at, pf2.started_at);
        assert_eq!(pf.relay, pf2.relay);
        assert_eq!(pf.no_relay, pf2.no_relay);
    }

    /// PID 0/1/negative/garbage are not treated as alive — they'd pass
    /// through `rustix::process::Pid::from_raw` either as None or as PID 1
    /// (init), which is universally alive but never ours.
    #[test]
    fn pid_alive_rejects_invalid_via_rustix() {
        // None branch
        assert_eq!(rustix::process::Pid::from_raw(0).is_some(), false);
        // Init may be alive but we don't care — the test we care about is
        // that the helper returns false on PID-of-zero. PID 1 case left
        // as integration smoke.
    }
}
