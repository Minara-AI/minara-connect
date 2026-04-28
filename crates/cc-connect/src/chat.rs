//! `cc-connect chat <ticket>` — join a Room, run a stdin REPL, and persist
//! every Message (sent + received) to the local log so the Hook can inject
//! them into Claude Code on the next prompt.
//!
//! Implements (most of) the join-side of PROTOCOL.md §3, §6.1, §8.
//!
//! v0.1 simplifications:
//!   - **No Backfill RPC** in this iteration — late-joiners see no history.
//!     The chat REPL prints `[chatroom] (joined late, no history)` once.
//!   - **No multi-Room subscription per Session** — one chat process per Room.
//!   - The hook contract (PID-file lifecycle) is honoured: PID is written
//!     *after* gossip join and removed on clean exit.
//!
//! Magic-moment flow this enables: Bob types in his `chat` REPL → his
//! Message goes via gossip → Alice's `chat` listener appends to her log →
//! Alice's next Claude prompt fires the Hook → Hook reads her log → injects
//! into Claude. No human action on Alice's side.

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use cc_connect_core::{
    identity::Identity,
    log_io,
    message::Message,
    ticket::decode_room_code,
};
use futures_lite::StreamExt;
use iroh::{address_lookup::memory::MemoryLookup, endpoint::presets, Endpoint, SecretKey};
use iroh_gossip::{
    api::Event,
    net::{Gossip, GOSSIP_ALPN},
    proto::TopicId,
};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::ticket_payload::TicketPayload;

pub fn run(ticket_str: &str) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    rt.block_on(run_async(ticket_str))
}

async fn run_async(ticket_str: &str) -> Result<()> {
    // 1. Decode ticket → topic + bootstrap peers.
    let payload_bytes = decode_room_code(ticket_str)
        .with_context(|| format!("decode room code: {ticket_str:.20}…"))?;
    let payload = TicketPayload::from_bytes(&payload_bytes)?;
    let topic = payload.topic;
    let bootstrap_peers = payload.peers;
    let topic_id_hex = topic_to_hex(&topic);

    // 2. Load Identity, derive iroh SecretKey (PROTOCOL.md §2 binding).
    let identity = load_identity()?;
    let pubkey_string = identity.pubkey_string();
    let secret_key = SecretKey::from_bytes(&identity.seed_bytes());

    // 3. Build endpoint with a MemoryLookup so we can register the bootstrap
    //    peers' addresses before subscribing.
    let memory_lookup = MemoryLookup::new();
    for peer in &bootstrap_peers {
        memory_lookup.add_endpoint_info(peer.clone());
    }
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .address_lookup(memory_lookup.clone())
        .bind()
        .await
        .context("bind iroh endpoint")?;

    // 4. Spawn gossip + Router accepting GOSSIP_ALPN.
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let _router = iroh::protocol::Router::builder(endpoint.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .spawn();

    // 5. Wait until online so subscription can talk to peers.
    endpoint.online().await;

    // 6. Subscribe to the gossip topic with the bootstrap peers as initial
    //    contacts. Returns a GossipTopic we split into (sender, receiver).
    let peer_ids: Vec<_> = bootstrap_peers.iter().map(|p| p.id).collect();
    let bootstrap_count = peer_ids.len();
    let topic_handle = gossip.subscribe_and_join(topic, peer_ids).await?;
    let (sender, mut receiver) = topic_handle.split();

    // 7. Write active-rooms PID file *after* bootstrap completes (PROTOCOL.md
    //    §8 + ADR-0003). Cleanup is via the guard's Drop.
    let pid_path = pid_file_path(&topic_id_hex)?;
    let _pid_guard = PidFileGuard::new(&pid_path)?;

    // 8. Open the local log (append + read-back) for both halves.
    let log_path = log_path_for(&topic_id_hex);
    let mut send_log = log_io::open_or_create_log(&log_path)?;

    println!();
    println!("Joined room: {} (peers: {})", &topic_id_hex[..12], bootstrap_count);
    println!("You are:     {}", &pubkey_string[..16]);
    println!("(no Backfill yet in v0.1 — late joiners see only new messages)");
    println!("Type to send. Ctrl-C / EOF to leave.");
    println!();

    // 9. Spawn gossip listener task. It writes incoming Messages to the same
    //    log file (with its own File handle); fcntl + single-syscall write
    //    keep concurrent appends safe.
    let listener_log_path = log_path.clone();
    let our_pubkey = pubkey_string.clone();
    let listener_handle = tokio::task::spawn(async move {
        let mut listener_log = match log_io::open_or_create_log(&listener_log_path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[chat] open listener log failed: {e:#}");
                return;
            }
        };
        while let Some(event) = receiver.next().await {
            let event = match event {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("[chat] gossip stream error: {e}");
                    continue;
                }
            };
            let payload: &[u8] = match &event {
                Event::Received(m) => m.content.as_ref(),
                _ => continue,
            };
            let msg = match Message::from_wire_bytes(payload) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[chat] dropped malformed Message: {e}");
                    continue;
                }
            };
            // Defence: don't echo our own broadcasts back into the log
            // (gossip can mirror them).
            if msg.author == our_pubkey {
                continue;
            }
            // Persist for the Hook to inject into Claude.
            if let Err(e) = log_io::append(&mut listener_log, &msg) {
                eprintln!("[chat] append incoming Message failed: {e:#}");
                continue;
            }
            // Tiny REPL display: short author + body, single-line.
            let nick_short: String = msg.author.chars().take(8).collect();
            let line: String = msg.body.replace(['\n', '\r', '\t'], " ");
            println!("[{nick_short}] {line}");
        }
    });

    // 10. REPL: read stdin lines, build canonical Messages, append + broadcast.
    let mut stdin_reader = tokio::io::BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    use tokio::io::AsyncBufReadExt;

    let repl_result: Result<()> = loop {
        line.clear();
        let n = match tokio::select! {
            r = stdin_reader.read_line(&mut line) => r,
            _ = tokio::signal::ctrl_c() => {
                println!("\n[chat] Ctrl-C — leaving room");
                break Ok(());
            }
        } {
            Ok(n) => n,
            Err(e) => break Err(anyhow!("read stdin: {e}")),
        };
        if n == 0 {
            // EOF.
            break Ok(());
        }
        let body = line.trim_end_matches(['\n', '\r']).to_string();
        if body.is_empty() {
            continue;
        }
        let msg = Message::new(&new_ulid(), pubkey_string.clone(), now_ms(), body)
            .context("build Message")?;
        // Local log first, then broadcast — if the broadcast fails the local
        // record is intact (PROTOCOL.md §6.1 step 3 ordering).
        if let Err(e) = log_io::append(&mut send_log, &msg) {
            eprintln!("[chat] append outgoing failed: {e:#}");
            continue;
        }
        let bytes = msg.to_canonical_json()?;
        if let Err(e) = sender.broadcast(Bytes::from(bytes)).await {
            eprintln!("[chat] broadcast failed: {e:#}");
        }
    };

    // 11. Cleanup. The pid_guard's Drop already removes the PID file.
    listener_handle.abort();
    drop(sender);
    drop(gossip);
    drop(endpoint);
    repl_result
}

/// Identity loader matching `host` / PROTOCOL.md §2.
fn load_identity() -> Result<Identity> {
    let path = identity_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    Identity::generate_or_load(&path)
}

fn identity_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".cc-connect").join("identity.key"))
}

fn log_path_for(topic_id_hex: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
    home.join(".cc-connect")
        .join("rooms")
        .join(topic_id_hex)
        .join("log.jsonl")
}

fn pid_file_path(topic_id_hex: &str) -> Result<PathBuf> {
    let uid = rustix::process::geteuid().as_raw();
    let dir = std::env::temp_dir()
        .join(format!("cc-connect-{uid}"))
        .join("active-rooms");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create_dir_all {}", dir.display()))?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    Ok(dir.join(format!("{topic_id_hex}.active")))
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

fn new_ulid() -> String {
    ulid::Ulid::new().to_string()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Owns the active-rooms PID file for the duration of `chat`.
struct PidFileGuard {
    path: PathBuf,
}

impl PidFileGuard {
    fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
        let pid = std::process::id().to_string();
        std::fs::write(path, pid).with_context(|| format!("write PID file {}", path.display()))?;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
