//! Persistent host daemon. Three user-visible subcommands plus one hidden
//! `host-bg-daemon` entry-point.
//!
//! ```text
//! cc-connect host-bg start [--relay <url>]
//!     ── spawns ourselves in a new session (setsid), waits for the daemon
//!        to print  "READY <topic_hex> <ticket>"  on stdout, prints the
//!        ticket to the user and exits 0. The daemon stays alive,
//!        re-parented to init.
//!
//! cc-connect host-bg list
//!     ── one line per running daemon (PID file under
//!        `~/.cc-connect/hosts/<topic_hex>.pid` is JSON; we parse + filter
//!        by liveness).
//!
//! cc-connect host-bg stop <topic_hex_prefix>
//!     ── unique-prefix match against the PID files, SIGTERM the daemon,
//!        wait for it to exit, remove the PID file.
//! ```
//!
//! Daemon lifecycle (PROTOCOL.md §8 outside the normal active-rooms flow —
//! these PID files live under HOME, not TMPDIR, since a daemon survives
//! reboots only if HOME is on persistent storage):
//!
//! - on start: write the JSON PID file under 0600 in 0700 dir, print READY,
//!   stop writing to stdout, host the topic until SIGTERM.
//! - on SIGTERM/SIGINT: clean up the PID file then exit 0.

use anyhow::{anyhow, bail, Context, Result};
use cc_connect_core::{log_io, message::Message, ticket::encode_room_code};
use futures_lite::StreamExt;
use iroh::{endpoint::RelayMode, Endpoint, RelayMap, SecretKey};
use iroh_gossip::{
    api::Event,
    net::{Gossip, GOSSIP_ALPN},
    proto::TopicId,
};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::backfill::{BackfillHandler, BACKFILL_ALPN};
use crate::ticket_payload::TicketPayload;

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_WAIT: Duration = Duration::from_secs(10);

/// JSON written to `~/.cc-connect/hosts/<topic_hex>.pid`.
#[derive(Debug, Serialize, Deserialize)]
struct HostPidFile {
    pid: u32,
    topic: String,
    ticket: String,
    started_at: i64,
    relay: Option<String>,
}

// ---------- `cc-connect host-bg start` ---------------------------------------

pub fn run_start(relay: Option<&str>) -> Result<()> {
    let exe = std::env::current_exe().context("locate self executable")?;
    let mut cmd = Command::new(exe);
    cmd.arg("host-bg-daemon");
    if let Some(r) = relay {
        cmd.arg("--relay").arg(r);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    // Detach from our session so closing this terminal doesn't SIGHUP the
    // daemon. Runs after fork, before exec — pre_exec contract.
    unsafe {
        cmd.pre_exec(|| {
            rustix::process::setsid()
                .map(|_| ())
                .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))
        });
    }

    let mut child = cmd.spawn().context("spawn daemon")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("daemon stdout pipe missing"))?;

    // Read the READY line on a side thread so we can timeout cleanly.
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
    let rest = trimmed
        .strip_prefix("READY ")
        .ok_or_else(|| anyhow!("daemon error or unexpected line: {trimmed:?}"))?;
    let mut parts = rest.splitn(2, ' ');
    let topic_hex = parts.next().ok_or_else(|| anyhow!("READY missing topic"))?;
    let ticket = parts
        .next()
        .ok_or_else(|| anyhow!("READY missing ticket"))?;

    println!();
    println!(
        "Daemon hosting room {} (pid {}):",
        &topic_hex[..12.min(topic_hex.len())],
        child.id()
    );
    println!();
    println!("    {ticket}");
    println!();
    println!(
        "Stop with:  cc-connect host-bg stop {}",
        &topic_hex[..12.min(topic_hex.len())]
    );

    // Don't reap the child — let it run, re-parented to init. Drop'ing
    // `Child` does NOT kill the process; std::mem::forget() avoids any
    // future reaping attempt.
    std::mem::forget(child);
    Ok(())
}

// ---------- `cc-connect host-bg list` ----------------------------------------

/// Snapshot of one running host-bg daemon, for inproc consumers (the TUI's
/// rooms overlay, primarily) that want the same info `host-bg list`
/// prints but as data instead of stdout.
#[derive(Debug, Clone)]
pub struct HostInfo {
    pub topic_hex: String,
    pub pid: u32,
    pub ticket: String,
    pub started_at: i64,
    pub relay: Option<String>,
}

/// List the live host-bg daemons. Sweeps stale `~/.cc-connect/hosts/*.pid`
/// files (process gone or malformed) as a side effect.
pub fn list_running() -> Result<Vec<HostInfo>> {
    let dir = hosts_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("readdir {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let topic_hex = match path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".pid"))
        {
            Some(s) => s.to_string(),
            None => continue,
        };
        let pf = match read_pid_file(&path) {
            Ok(p) => p,
            Err(_) => {
                let _ = std::fs::remove_file(&path);
                continue;
            }
        };
        if !pid_alive(pf.pid)? {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        out.push(HostInfo {
            topic_hex,
            pid: pf.pid,
            ticket: pf.ticket,
            started_at: pf.started_at,
            relay: pf.relay,
        });
    }
    Ok(out)
}

pub fn run_list() -> Result<()> {
    let hosts = list_running()?;
    if hosts.is_empty() {
        println!("(no daemons running)");
        return Ok(());
    }
    let now = now_ms();
    for h in hosts {
        let uptime = (now - h.started_at).max(0);
        println!(
            "{topic} pid={pid} uptime={up}s relay={relay}",
            topic = &h.topic_hex[..12.min(h.topic_hex.len())],
            pid = h.pid,
            up = uptime / 1000,
            relay = h.relay.as_deref().unwrap_or("(default)"),
        );
    }
    Ok(())
}

// ---------- `cc-connect host-bg stop <topic_prefix>` -------------------------

pub fn run_stop(topic_prefix: &str) -> Result<()> {
    let dir = hosts_dir();
    if !dir.exists() {
        bail!("no daemons running");
    }
    let mut matches: Vec<(String, PathBuf, HostPidFile)> = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let topic_hex = match path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".pid"))
        {
            Some(s) => s.to_string(),
            None => continue,
        };
        if !topic_hex.starts_with(topic_prefix) {
            continue;
        }
        let pf = match read_pid_file(&path) {
            Ok(p) => p,
            Err(_) => continue,
        };
        matches.push((topic_hex, path, pf));
    }
    match matches.len() {
        0 => bail!("no daemon matches prefix {topic_prefix:?}"),
        1 => {}
        n => bail!(
            "{n} daemons match {topic_prefix:?}: {:?}",
            matches.iter().map(|m| &m.0).collect::<Vec<_>>()
        ),
    }
    let (_topic_hex, pid_path, pf) = matches.into_iter().next().unwrap();
    let pid_obj = rustix::process::Pid::from_raw(pf.pid as i32)
        .ok_or_else(|| anyhow!("invalid pid {}", pf.pid))?;
    if let Err(e) = rustix::process::kill_process(pid_obj, rustix::process::Signal::TERM) {
        if e == rustix::io::Errno::SRCH {
            // Already gone — sweep.
            let _ = std::fs::remove_file(&pid_path);
            println!("daemon was already gone; PID file cleaned up");
            return Ok(());
        }
        return Err(anyhow!("kill_process({}): {e}", pf.pid));
    }
    // Wait up to STOP_WAIT for the daemon to exit (PID file removed by it).
    let start = std::time::Instant::now();
    while start.elapsed() < STOP_WAIT {
        if !pid_path.exists() {
            println!("daemon stopped");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Daemon did not clean up; do it ourselves.
    let _ = std::fs::remove_file(&pid_path);
    println!("daemon did not clean up within {STOP_WAIT:?}; forced PID-file removal");
    Ok(())
}

// ---------- daemon entry-point (`cc-connect host-bg-daemon`) -----------------

pub fn run_daemon(relay: Option<&str>) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build daemon tokio runtime")?;
    rt.block_on(daemon_async(relay))
}

async fn daemon_async(relay: Option<&str>) -> Result<()> {
    // The daemon uses an EPHEMERAL random identity, NOT the user's
    // `~/.cc-connect/identity.key`. Reason: when this daemon and a
    // chat_session run on the same machine sharing one identity, both
    // iroh endpoints claim the same NodeId. iroh-gossip then delivers
    // each gossip event to whichever endpoint subscribed first — which
    // is host-bg, a passive bootstrap node with no chat listener — and
    // the bytes get black-holed before reaching the user's TUI. Giving
    // host-bg its own NodeId makes it a distinct peer in the mesh, so
    // gossip events fan out to every subscriber correctly.
    let mut secret_seed = [0u8; 32];
    getrandom::getrandom(&mut secret_seed)
        .map_err(|e| anyhow!("OS random for daemon secret key: {e}"))?;
    let secret_key = SecretKey::from_bytes(&secret_seed);

    let mut builder = Endpoint::builder(iroh::endpoint::presets::N0).secret_key(secret_key);
    if let Some(url) = relay {
        let map =
            RelayMap::try_from_iter([url]).map_err(|e| anyhow!("RELAY_URL_INVALID: {url}: {e}"))?;
        builder = builder.relay_mode(RelayMode::Custom(map));
    }
    let endpoint = builder.bind().await.context("bind iroh endpoint")?;

    let mut topic_bytes = [0u8; 32];
    getrandom::getrandom(&mut topic_bytes).map_err(|e| anyhow!("OS random for topic: {e}"))?;
    let topic = TopicId::from_bytes(topic_bytes);
    let topic_hex_for_log = topic_to_hex(&topic);
    let log_path = home_dir()
        .join(".cc-connect")
        .join("rooms")
        .join(&topic_hex_for_log)
        .join("log.jsonl");

    let gossip = Gossip::builder().spawn(endpoint.clone());
    // Register BACKFILL_ALPN against the shared log so joiners' backfill
    // RPC against the daemon's NodeId (the only one in the ticket)
    // returns history. Without this, the joiner's dial fails ALPN
    // negotiation with "peer doesn't support any known protocol" and the
    // joined-late marker fires every time. The log path is shared with
    // the user's TUI / chat-daemon (same `~/.cc-connect/rooms/<topic>/`
    // root); fcntl locking in log_io serialises writes.
    let backfill_handler = BackfillHandler::new(log_path.clone());
    let _router = iroh::protocol::Router::builder(endpoint.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .accept(BACKFILL_ALPN, backfill_handler)
        .spawn();

    endpoint.online().await;

    // Subscribe to our own topic so joiners can bootstrap (the fix from
    // 64eabb5; otherwise `subscribe_and_join` on the joiner side hangs).
    let topic_handle = gossip.subscribe(topic, vec![]).await?;
    // Active-drain the receiver (PROTOCOL §6.1 forwarding correctness):
    // a passive subscriber that never reads its receiver causes
    // iroh-gossip to back-pressure and drop forwards, so peers connected
    // to the daemon as their only bootstrap can lose messages
    // asymmetrically. Drain in a background task and append unseen
    // Messages to the shared log.jsonl so the user's TUI hook still sees
    // them on the next prompt even if its own gossip listener missed
    // them. Dedup by id (PROTOCOL §5) keeps the log consistent.
    spawn_gossip_drain(topic_handle, log_path.clone());

    let our_addr = endpoint.addr();
    let payload = TicketPayload {
        topic,
        peers: vec![our_addr],
    };
    let payload_bytes = payload.to_bytes()?;
    let ticket = encode_room_code(&payload_bytes);
    let topic_hex = topic_to_hex(&topic);

    // Write PID file BEFORE printing READY — once parent sees READY, it
    // exits and the user starts using the daemon.
    let pid_path = pid_file_path(&topic_hex);
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let pf = HostPidFile {
        pid: std::process::id(),
        topic: topic_hex.clone(),
        ticket: ticket.clone(),
        started_at: now_ms(),
        relay: relay.map(|s| s.to_string()),
    };
    let pid_json = serde_json::to_string(&pf).context("serialize PID file")?;
    std::fs::write(&pid_path, format!("{pid_json}\n"))
        .with_context(|| format!("write {}", pid_path.display()))?;
    let _ = std::fs::set_permissions(&pid_path, std::fs::Permissions::from_mode(0o600));

    // Tell the parent we're alive. Single line, then never write to stdout
    // again (parent closed the read end almost immediately).
    {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "READY {topic_hex} {ticket}").context("write READY")?;
        let _ = stdout.flush();
    }

    // Park until SIGTERM/SIGINT.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("install SIGTERM handler")?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .context("install SIGINT handler")?;
    tokio::select! {
        _ = sigterm.recv() => {},
        _ = sigint.recv() => {},
    }

    // Clean up. The gossip-drain task ends when the gossip handle drops.
    let _ = std::fs::remove_file(&pid_path);
    drop(gossip);
    drop(endpoint);
    Ok(())
}

/// Active receiver drain for the daemon's topic subscription. Without
/// this, iroh-gossip treats the daemon as a back-pressured peer and its
/// forwards become unreliable — a joiner whose only bootstrap is the
/// daemon may never see another peer's broadcasts even though the gossip
/// mesh is nominally connected (asymmetric visibility, observed pre-fix).
///
/// We append every well-formed Message to the shared `log.jsonl` for the
/// topic. `log_io::append` is fcntl-locked and dedup-safe, so concurrent
/// writes from the user's TUI / chat-daemon for the same room don't
/// corrupt. The daemon never displays anything — this drain exists for
/// (a) gossip-protocol back-pressure, (b) keeping the log warm so a
/// freshly-launched TUI's hook can render history immediately.
fn spawn_gossip_drain(
    handle: iroh_gossip::api::GossipTopic,
    log_path: PathBuf,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (_sender, mut receiver) = handle.split();
        let mut log_file = match log_io::open_or_create_log(&log_path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[host-bg] open log {}: {e:#}", log_path.display());
                return;
            }
        };
        while let Some(event) = receiver.next().await {
            let payload: Vec<u8> = match event {
                Ok(Event::Received(m)) => m.content.to_vec(),
                Ok(_) => continue,
                Err(e) => {
                    eprintln!("[host-bg] gossip stream error: {e}");
                    continue;
                }
            };
            let msg = match Message::from_wire_bytes(&payload) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[host-bg] dropped malformed Message: {e}");
                    continue;
                }
            };
            if let Err(e) = log_io::append(&mut log_file, &msg) {
                eprintln!("[host-bg] append Message {} failed: {e:#}", msg.id);
            }
        }
    })
}

// ---------- helpers ---------------------------------------------------------

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn hosts_dir() -> PathBuf {
    home_dir().join(".cc-connect").join("hosts")
}

fn pid_file_path(topic_hex: &str) -> PathBuf {
    hosts_dir().join(format!("{topic_hex}.pid"))
}

fn read_pid_file(path: &Path) -> Result<HostPidFile> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let pf: HostPidFile =
        serde_json::from_str(raw.trim()).with_context(|| format!("parse {}", path.display()))?;
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

fn topic_to_hex(topic: &TopicId) -> String {
    let bytes = topic.as_bytes();
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
