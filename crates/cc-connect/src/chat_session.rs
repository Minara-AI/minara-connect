//! Library-form chat session — drives a Room's gossip + iroh-blobs +
//! local-log lifecycle behind mpsc channels, so both the existing
//! `cc-connect chat` REPL binary and the upcoming TUI can share one
//! implementation.
//!
//! Caller pattern:
//!
//! ```ignore
//! let mut handle = chat_session::spawn(cfg).await?;
//! // Pull display lines:
//! while let Some(line) = handle.display_rx.recv().await { ... }
//! // Push input lines (treated as stdin lines, so `/drop <path>` works):
//! handle.input_tx.send("hello".into()).await?;
//! // Drop input_tx OR await handle.join to shut the session down.
//! ```

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use cc_connect_core::{
    drop_safety::{self, DropSafety},
    identity::Identity,
    log_io,
    message::Message,
    rate_limit::{RateLimitDecision, RateLimiter},
    ticket::decode_room_code,
};
use futures_lite::StreamExt;
use iroh::{
    address_lookup::memory::MemoryLookup, endpoint::presets, endpoint::RelayMode, Endpoint,
    PublicKey, RelayMap, SecretKey,
};
use iroh_blobs::{store::mem::MemStore, BlobsProtocol, Hash};
use iroh_gossip::{
    api::Event,
    net::{Gossip, GOSSIP_ALPN},
    proto::TopicId,
};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

use crate::backfill::{try_backfill_from, BackfillHandler, BackfillOutcome, BACKFILL_ALPN};
use crate::ticket_payload::TicketPayload;

/// What the caller has to provide to start a session.
pub struct ChatSessionConfig {
    pub ticket: String,
    pub no_relay: bool,
    pub relay: Option<String>,
}

/// One renderable unit emitted by the running session.
#[derive(Debug, Clone)]
pub enum DisplayLine {
    /// Free-form system text — banner lines like "Joined room: …".
    System(String),
    /// Backfill marker, e.g. "[chatroom] (backfilled 7 messages from peer)"
    /// or "[chatroom] (joined late, no history available)".
    Marker(String),
    /// A chat or file_drop Message we received from a remote peer. The
    /// `mentions_me` flag is true when the body contains `@<self_nick>`,
    /// `@cc`, `@claude`, `@all`, or `@here` (case-insensitive).
    Incoming {
        nick_short: String,
        body: String,
        mentions_me: bool,
    },
    /// Our own /drop confirmation echo (e.g. "[chat] dropped foo.svg (4096 bytes)").
    Echo(String),
    /// Soft, non-fatal error visible to the user (replaces eprintln!).
    Warn(String),
}

/// Pure function: does `body` mention "me"?
///
/// Recognised tokens (case-insensitive substring match):
///   - `@<self_nick>` — only checked if `self_nick` is `Some` and non-empty.
///   - `@cc`, `@claude` — addresses any/all Claude Code instances.
///   - `@all`, `@here` — broadcast attention.
pub fn line_mentions_me(body: &str, self_nick: Option<&str>) -> bool {
    let lower = body.to_ascii_lowercase();
    if lower.contains("@cc")
        || lower.contains("@claude")
        || lower.contains("@all")
        || lower.contains("@here")
    {
        return true;
    }
    if let Some(nick) = self_nick.filter(|s| !s.is_empty()) {
        let token = format!("@{}", nick.to_ascii_lowercase());
        if lower.contains(&token) {
            return true;
        }
    }
    false
}

/// One @-mention event, surfaced by the gossip listener and consumable by
/// `cc_wait_for_mention` (IPC action `wait_for_mention`). Serialised
/// straight into the IPC response so cc-connect-mcp can hand it back to
/// Claude verbatim.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MentionEvent {
    pub id: String,
    pub ts: i64,
    pub nick: String,
    pub body: String,
}

/// Capacity of the recent-mentions ring buffer + the broadcast channel.
/// Sized so a Claude that's slow re-arming `cc_wait_for_mention` doesn't
/// lose events delivered between two calls.
const MENTION_BUFFER_CAP: usize = 32;

/// Shared state for the wait_for_mention IPC action: a broadcast channel
/// that wakes any waiter the moment a mention lands, plus a small ring
/// buffer that lets a re-arming caller (with a `since_id` watermark) pick
/// up mentions that arrived between two calls.
pub(crate) struct MentionState {
    tx: tokio::sync::broadcast::Sender<MentionEvent>,
    recent: std::sync::Mutex<std::collections::VecDeque<MentionEvent>>,
}

impl MentionState {
    fn new() -> std::sync::Arc<Self> {
        let (tx, _) = tokio::sync::broadcast::channel(MENTION_BUFFER_CAP);
        std::sync::Arc::new(Self {
            tx,
            recent: std::sync::Mutex::new(std::collections::VecDeque::with_capacity(
                MENTION_BUFFER_CAP,
            )),
        })
    }

    /// Record an @-mention. Buffer cap-evicts oldest; broadcast send is
    /// best-effort (no waiters → ignored).
    fn push(&self, ev: MentionEvent) {
        {
            let mut buf = self.recent.lock().expect("mention buffer poisoned");
            if buf.len() >= MENTION_BUFFER_CAP {
                buf.pop_front();
            }
            buf.push_back(ev.clone());
        }
        let _ = self.tx.send(ev);
    }

    /// Find the oldest buffered mention strictly newer than `cutoff` (by
    /// ULID lex comparison, which matches chronological order). Returns
    /// `None` if `cutoff` is `None` (the caller wants only new events,
    /// not buffered replay) or if the buffer holds nothing newer.
    fn snapshot_after(&self, cutoff: Option<&str>) -> Option<MentionEvent> {
        let cutoff = cutoff?;
        let buf = self.recent.lock().expect("mention buffer poisoned");
        buf.iter()
            .find(|e| e.id.as_str() > cutoff)
            .cloned()
    }
}

/// Handle returned from [`spawn`].
pub struct ChatHandle {
    /// Read display lines from the session (chat scrollback).
    pub display_rx: mpsc::Receiver<DisplayLine>,
    /// Send a line of user input ("hello" or "/drop ./file"). Closing this
    /// (drop) makes the session exit cleanly.
    pub input_tx: mpsc::Sender<String>,
    /// Joins the underlying session task.
    pub join: tokio::task::JoinHandle<Result<()>>,
}

/// Where an outbound chat line came from. Used to pick the sender nick:
/// the local user types as `self_nick`, MCP-driven Claude appears as
/// `<self_nick>-cc` so peers can tell the AI apart from the human.
#[derive(Copy, Clone, Debug)]
enum InputSource {
    Local,
    Mcp,
}

/// Boot a chat session as a tokio task. The task does the iroh + gossip +
/// blobs setup, the backfill RPC, the active-rooms PID file, then loops on
/// the input channel until it closes.
pub async fn spawn(cfg: ChatSessionConfig) -> Result<ChatHandle> {
    let (display_tx, display_rx) = mpsc::channel::<DisplayLine>(100);
    let (input_tx, input_rx) = mpsc::channel::<String>(32);
    // MCP (cc-connect-mcp via IPC) writes to its own channel so the send
    // loop can tag those lines as InputSource::Mcp and rebrand the nick
    // to `<self_nick>-cc`.
    let (mcp_tx, mcp_rx) = mpsc::channel::<String>(32);
    // Clone of `input_tx` for the IPC server: when chat-ui marks a
    // payload as `source: "human"` the IPC dispatch routes onto this
    // Sender, so it lands in `input_rx` and the send loop tags it as
    // InputSource::Local — peers see the human's nick (no `-cc` suffix)
    // and owner @-mention wakeups fire correctly.
    let local_input_tx = input_tx.clone();
    let join = tokio::spawn(run_session(
        cfg,
        display_tx,
        input_rx,
        local_input_tx,
        mcp_tx,
        mcp_rx,
    ));
    Ok(ChatHandle {
        display_rx,
        input_tx,
        join,
    })
}

async fn run_session(
    cfg: ChatSessionConfig,
    display_tx: mpsc::Sender<DisplayLine>,
    mut input_rx: mpsc::Receiver<String>,
    local_input_tx: mpsc::Sender<String>,
    mcp_input_tx: mpsc::Sender<String>,
    mut mcp_input_rx: mpsc::Receiver<String>,
) -> Result<()> {
    // 1. Decode ticket → topic + bootstrap peers.
    let payload_bytes = decode_room_code(&cfg.ticket)
        .with_context(|| format!("decode room code: {:.20}…", cfg.ticket))?;
    let payload = TicketPayload::from_bytes(&payload_bytes)?;
    let topic = payload.topic;
    let bootstrap_peers = payload.peers;
    let topic_id_hex = topic_to_hex(&topic);

    // 2. Identity → SecretKey (PROTOCOL.md §2 binding).
    let identity = load_identity()?;
    let pubkey_string = identity.pubkey_string();
    let secret_key = SecretKey::from_bytes(&identity.seed_bytes());

    // 2.5. User's self-declared nick (best-effort; missing config = no nick).
    let self_nick = load_self_nick();

    // 3. Endpoint with MemoryLookup pre-populated with the bootstrap peers.
    let memory_lookup = MemoryLookup::new();
    for peer in &bootstrap_peers {
        memory_lookup.add_endpoint_info(peer.clone());
    }
    let mut builder = Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .address_lookup(memory_lookup.clone());
    if cfg.no_relay {
        builder = builder.relay_mode(RelayMode::Disabled);
    } else if let Some(url) = cfg.relay.as_deref() {
        let map = RelayMap::try_from_iter([url])
            .map_err(|e| anyhow!("RELAY_URL_INVALID: {url}: {e}"))?;
        builder = builder.relay_mode(RelayMode::Custom(map));
    }
    let endpoint = builder.bind().await.context("bind iroh endpoint")?;

    // 4. Gossip + iroh-blobs MemStore + Router (gossip / backfill / blobs ALPNs).
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let store = MemStore::new();
    let blobs_proto = BlobsProtocol::new(&store, None);
    let log_path = log_path_for(&topic_id_hex);
    let backfill_handler = BackfillHandler::new(log_path.clone());
    let _router = iroh::protocol::Router::builder(endpoint.clone())
        .accept(GOSSIP_ALPN, gossip.clone())
        .accept(BACKFILL_ALPN, backfill_handler)
        .accept(iroh_blobs::ALPN, blobs_proto)
        .spawn();

    // 5. Wait for relay home unless --no-relay.
    if !cfg.no_relay {
        endpoint.online().await;
    }

    // 6. Subscribe + wait until we're meshed with a peer.
    let peer_ids: Vec<_> = bootstrap_peers.iter().map(|p| p.id).collect();
    let bootstrap_count = peer_ids.len();
    let topic_handle = gossip.subscribe_and_join(topic, peer_ids).await?;
    let (sender, mut receiver) = topic_handle.split();

    // 7. Backfill from the first peer (PROTOCOL.md §6.2).
    let backfill_marker = if let Some(first_peer) = bootstrap_peers.first() {
        if first_peer.id == endpoint.id() {
            None
        } else {
            let files_dir = files_dir_for(&topic_id_hex);
            match try_backfill_from(
                &endpoint,
                &store,
                first_peer,
                None,
                &log_path,
                &files_dir,
            )
            .await
            {
                BackfillOutcome::Filled { appended } if appended > 0 => Some(format!(
                    "[chatroom] (backfilled {appended} message{} from peer)",
                    if appended == 1 { "" } else { "s" }
                )),
                BackfillOutcome::Filled { .. } | BackfillOutcome::Empty => None,
                BackfillOutcome::Timeout => {
                    Some("[chatroom] (joined late, no history available)".to_string())
                }
                BackfillOutcome::Failed(msg) => {
                    let _ = display_tx
                        .send(DisplayLine::Warn(format!("[chat] backfill failed: {msg}")))
                        .await;
                    Some("[chatroom] (joined late, no history available)".to_string())
                }
            }
        }
    } else {
        None
    };

    // 8. Active-rooms PID file (PROTOCOL §8 + ADR-0003).
    let pid_path = pid_file_path(&topic_id_hex)?;
    let _pid_guard = PidFileGuard::new(&pid_path)?;

    // 8.0. Mention state for the `wait_for_mention` IPC action — shared
    // between the gossip listener (producer) and IPC handlers (consumers).
    let mention_state = MentionState::new();

    // 8.5. IPC unix-socket server. Lets cc-connect-mcp (and any other
    //      local helper) drive this session — sending chat lines on
    //      Claude Code's behalf, querying recent log, etc.
    let (ipc_sock, ipc_marker) = ipc_socket_path(&topic_id_hex)?;
    let _ipc_guard = IpcSocketGuard::new(&ipc_sock, &ipc_marker);
    let ipc_listener = match tokio::net::UnixListener::bind(&ipc_sock) {
        Ok(l) => Some(l),
        Err(e) => {
            let _ = display_tx
                .send(DisplayLine::Warn(format!(
                    "[chat] IPC socket bind failed ({}): {e}",
                    ipc_sock.display()
                )))
                .await;
            None
        }
    };
    let ipc_handle = if let Some(listener) = ipc_listener {
        let _ = std::fs::set_permissions(
            &ipc_sock,
            std::fs::Permissions::from_mode(0o600),
        );
        let mcp_input_tx = mcp_input_tx.clone();
        let local_input_tx = local_input_tx.clone();
        let ipc_log_path = log_path.clone();
        let ipc_files_dir = files_dir_for(&topic_id_hex);
        let ipc_mention_state = mention_state.clone();
        Some(tokio::spawn(async move {
            ipc_server_loop(
                listener,
                local_input_tx,
                mcp_input_tx,
                ipc_log_path,
                ipc_files_dir,
                ipc_mention_state,
            )
            .await
        }))
    } else {
        None
    };

    // 9. Open the local log for the send half.
    let mut send_log = log_io::open_or_create_log(&log_path)?;

    // 10. Banner — replaces the println! header from the old chat REPL.
    let _ = display_tx
        .send(DisplayLine::System(format!(
            "Joined room: {} (peers: {})",
            &topic_id_hex[..12],
            bootstrap_count
        )))
        .await;
    let _ = display_tx
        .send(DisplayLine::System(format!(
            "You are:     {}",
            &pubkey_string[..16]
        )))
        .await;
    if let Some(marker) = backfill_marker {
        let _ = display_tx.send(DisplayLine::Marker(marker)).await;
    }

    // 11. Spawn the gossip listener task. It owns its own File handle to the
    //     log (fcntl + single-syscall append makes concurrent writes safe).
    let listener_log_path = log_path.clone();
    let listener_files_dir = files_dir_for(&topic_id_hex);
    let listener_store = store.clone();
    let listener_endpoint = endpoint.clone();
    let our_pubkey = pubkey_string.clone();
    let listener_display = display_tx.clone();
    let listener_self_nick = self_nick.clone();
    let listener_handle = tokio::task::spawn(async move {
        let mut listener_log = match log_io::open_or_create_log(&listener_log_path) {
            Ok(f) => f,
            Err(e) => {
                let _ = listener_display
                    .send(DisplayLine::Warn(format!(
                        "[chat] open listener log failed: {e:#}"
                    )))
                    .await;
                return;
            }
        };
        // Per-author flood guard. Uses receiver-side wall clock so a peer
        // can't bypass the limit by lying about `msg.ts`. See SECURITY.md
        // §"Per-author flooding" — the limit is per identity, not per
        // human; sybil resistance is out of scope for v0.1.
        let mut rate_limiter = RateLimiter::new();
        while let Some(event) = receiver.next().await {
            let event = match event {
                Ok(e) => e,
                Err(e) => {
                    let _ = listener_display
                        .send(DisplayLine::Warn(format!("[chat] gossip stream error: {e}")))
                        .await;
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
                    let _ = listener_display
                        .send(DisplayLine::Warn(format!("[chat] dropped malformed Message: {e}")))
                        .await;
                    continue;
                }
            };
            // Don't echo our own broadcasts back into the log.
            if msg.author == our_pubkey {
                continue;
            }
            match rate_limiter.check_and_record(&msg.author, now_ms()) {
                RateLimitDecision::Allow => {}
                RateLimitDecision::Drop { warn } => {
                    if warn {
                        let nick_short: String = match msg.nick.as_deref() {
                            Some(n) if !n.is_empty() => n.to_string(),
                            _ => msg.author.chars().take(8).collect(),
                        };
                        let _ = listener_display
                            .send(DisplayLine::Warn(format!(
                                "[chat] rate-limited @{nick_short} (>{}/{}s); dropping further messages from this author until they slow down",
                                cc_connect_core::rate_limit::RATE_LIMIT_MAX_PER_WINDOW,
                                cc_connect_core::rate_limit::RATE_LIMIT_WINDOW_MS / 1000,
                            )))
                            .await;
                    }
                    continue;
                }
            }
            // file_drop: dial the author's NodeId via iroh-blobs to fetch the
            // bytes, then export them locally.
            if msg.kind == cc_connect_core::message::KIND_FILE_DROP {
                if let Err(e) = fetch_and_export_blob(
                    &listener_store,
                    &listener_endpoint,
                    &msg,
                    &listener_files_dir,
                )
                .await
                {
                    let _ = listener_display
                        .send(DisplayLine::Warn(format!(
                            "[chat] file_drop blob fetch failed for {}: {e:#}",
                            msg.id
                        )))
                        .await;
                    continue;
                }
            }
            // Persist to the log (the Hook reads this).
            if let Err(e) = log_io::append(&mut listener_log, &msg) {
                let _ = listener_display
                    .send(DisplayLine::Warn(format!(
                        "[chat] append incoming Message failed: {e:#}"
                    )))
                    .await;
                continue;
            }
            // Best-effort INDEX.md append — non-fatal if it fails.
            if msg.kind == cc_connect_core::message::KIND_FILE_DROP {
                if let Err(e) = append_file_index_entry(&listener_files_dir, &msg) {
                    let _ = listener_display
                        .send(DisplayLine::Warn(format!(
                            "[chat] INDEX.md append failed: {e:#}"
                        )))
                        .await;
                }
            }
            // Prefer the sender's self-declared nick (v0.2 field) over the
            // pubkey-prefix fallback. Receivers see the same name across
            // peers as the sender intended.
            let nick_short: String = match msg.nick.as_deref() {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => msg.author.chars().take(8).collect(),
            };
            let body: String = if msg.kind == cc_connect_core::message::KIND_FILE_DROP {
                format!("dropped {}", msg.body)
            } else {
                msg.body.replace(['\n', '\r', '\t'], " ")
            };
            // mentions_me here is purely the VISUAL flag (UI styles peer
            // @-mentions in red and tags them `(@me)` so the user can see
            // someone tried to reach their AI). It is intentionally NOT a
            // wake signal — under the owner-only rule, peer @-mentions
            // must not auto-trigger our embedded Claude. Owner-driven
            // wake-ups are pushed from the send path instead.
            let mentions_me = line_mentions_me(&body, listener_self_nick.as_deref());
            let _ = listener_display
                .send(DisplayLine::Incoming {
                    nick_short,
                    body,
                    mentions_me,
                })
                .await;
        }
    });

    // 12. Send loop — merge user-typed input (input_rx) and MCP-driven
    // input (mcp_input_rx). Each branch tags its source so we can rewrite
    // the nick: locals send as `self_nick`, MCP sends as `<self_nick>-cc`.
    //
    // Outgoing rate limit. Symmetric with the receiver-side limiter above:
    // a buggy chat-ui or a runaway local Claude can't flood the room
    // either. Both InputSource::Local AND InputSource::Mcp pass through
    // it because in the chat-daemon architecture chat-ui sends arrive as
    // Mcp source (it talks via the IPC `send` action like MCP does), and
    // we want both kinds of "us" rate-limited.
    let mut send_rate_limiter = RateLimiter::new();
    let result: Result<()> = loop {
        let (line, source) = tokio::select! {
            biased;
            l = input_rx.recv() => match l {
                Some(s) => (s, InputSource::Local),
                None => break Ok(()), // caller dropped input_tx → clean shutdown
            },
            l = mcp_input_rx.recv() => match l {
                Some(s) => (s, InputSource::Mcp),
                None => continue, // MCP server gone; the user channel still drives shutdown
            },
        };
        let body = line.trim_end_matches(['\n', '\r']).to_string();
        if body.is_empty() {
            continue;
        }

        // Outgoing flood guard. We key on our own pubkey + a per-source
        // suffix so Local and Mcp share the same window — a person typing
        // 25 lines in chat-ui plus their AI typing 10 should still trip
        // the same 30-msg ceiling for the room.
        match send_rate_limiter.check_and_record(&pubkey_string, now_ms()) {
            RateLimitDecision::Allow => {}
            RateLimitDecision::Drop { warn } => {
                if warn {
                    let _ = display_tx
                        .send(DisplayLine::Warn(format!(
                            "[chat] outgoing rate-limit hit (>{}/{}s); message dropped — slow down",
                            cc_connect_core::rate_limit::RATE_LIMIT_MAX_PER_WINDOW,
                            cc_connect_core::rate_limit::RATE_LIMIT_WINDOW_MS / 1000,
                        )))
                        .await;
                }
                continue;
            }
        }

        // Effective nick for this send. Local = the user; Mcp = the user's
        // embedded Claude (we suffix `-cc` so peers can tell them apart).
        let base_nick: String = self_nick
            .clone()
            .unwrap_or_else(|| pubkey_string.chars().take(8).collect::<String>());
        let echo_nick = match source {
            InputSource::Local => base_nick.clone(),
            InputSource::Mcp => format!("{base_nick}-cc"),
        };
        let nick_for_msg: Option<String> = match source {
            InputSource::Local => self_nick.clone(),
            InputSource::Mcp => Some(format!("{base_nick}-cc")),
        };

        let msg = if let Some(path_str) = body.strip_prefix("/drop ") {
            match build_file_drop(&store, path_str.trim(), &pubkey_string, &topic_id_hex).await {
                Ok(m) => {
                    let m = m
                        .with_nick(nick_for_msg.clone())
                        .context("attach nick to file_drop")?;
                    let _ = display_tx
                        .send(DisplayLine::Echo(format!(
                            "[chat] dropped {} ({} bytes)",
                            m.body,
                            m.blob_size.unwrap_or(0)
                        )))
                        .await;
                    m
                }
                Err(e) => {
                    let _ = display_tx
                        .send(DisplayLine::Warn(format!("[chat] /drop failed: {e:#}")))
                        .await;
                    continue;
                }
            }
        } else if body.starts_with('/') {
            let _ = display_tx
                .send(DisplayLine::Warn(
                    "[chat] unknown slash command. Available: `/drop <path>`. Type plain text to chat."
                        .to_string(),
                ))
                .await;
            continue;
        } else {
            // Echo our own chat line into the scrollback so the user sees what
            // they sent (the listener filters out msg.author == our_pubkey to
            // avoid duplicate gossip echoes — that's the correct dedup, but it
            // also hides our own send, which is wrong UX).
            let _ = display_tx
                .send(DisplayLine::Echo(format!("[{echo_nick}] {body}")))
                .await;
            Message::new(&new_ulid(), pubkey_string.clone(), now_ms(), body)
                .context("build Message")?
                .with_nick(nick_for_msg.clone())
                .context("attach nick to chat")?
        };

        if let Err(e) = log_io::append(&mut send_log, &msg) {
            let _ = display_tx
                .send(DisplayLine::Warn(format!("[chat] append outgoing failed: {e:#}")))
                .await;
            continue;
        }
        // Owner @-mention wake-up: only the human user typing into chat
        // (InputSource::Local) addressing their own AI counts as a wake
        // signal for `cc_wait_for_mention`. Peer messages are excluded
        // upstream (listener no longer pushes), and the AI's own MCP-driven
        // sends are excluded here — otherwise an `@cc` in the AI's own
        // broadcast would self-trigger an infinite loop.
        if matches!(source, InputSource::Local)
            && msg.kind != cc_connect_core::message::KIND_FILE_DROP
            && line_mentions_me(&msg.body, self_nick.as_deref())
        {
            mention_state.push(MentionEvent {
                id: msg.id.clone(),
                ts: msg.ts,
                nick: echo_nick.clone(),
                body: msg.body.clone(),
            });
        }
        // INDEX.md append for our own file_drops (best-effort).
        if msg.kind == cc_connect_core::message::KIND_FILE_DROP {
            let files_dir = files_dir_for(&topic_id_hex);
            if let Err(e) = append_file_index_entry(&files_dir, &msg) {
                let _ = display_tx
                    .send(DisplayLine::Warn(format!(
                        "[chat] INDEX.md append failed: {e:#}"
                    )))
                    .await;
            }
        }
        let bytes = msg.to_canonical_json()?;
        if let Err(e) = sender.broadcast(Bytes::from(bytes)).await {
            let _ = display_tx
                .send(DisplayLine::Warn(format!("[chat] broadcast failed: {e:#}")))
                .await;
        }
    };

    // 13. Cleanup. PidFileGuard's + IpcSocketGuard's Drop remove the
    //     active-rooms file + the unix socket.
    if let Some(h) = ipc_handle {
        h.abort();
    }
    listener_handle.abort();
    drop(sender);
    drop(gossip);
    drop(endpoint);
    result
}

// ---------- helpers (moved unchanged from chat.rs) ---------------------------

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
    rooms_dir(topic_id_hex).join("log.jsonl")
}

fn files_dir_for(topic_id_hex: &str) -> PathBuf {
    rooms_dir(topic_id_hex).join("files")
}

fn rooms_dir(topic_id_hex: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    home.join(".cc-connect").join("rooms").join(topic_id_hex)
}

/// Append a line to `<files_dir>/INDEX.md` summarising a file_drop. Used
/// by both the listener (incoming drops) and the send path (our own /drop).
/// The INDEX.md is human-readable and also injected into the hook output
/// so Claude has a stable reference of every file in the room.
fn append_file_index_entry(files_dir: &Path, msg: &Message) -> Result<()> {
    if msg.kind != cc_connect_core::message::KIND_FILE_DROP {
        return Ok(());
    }
    std::fs::create_dir_all(files_dir)
        .with_context(|| format!("create_dir_all {}", files_dir.display()))?;
    let _ = std::fs::set_permissions(files_dir, std::fs::Permissions::from_mode(0o700));
    let path = files_dir.join("INDEX.md");
    let nick = match msg.nick.as_deref() {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => msg.author.chars().take(8).collect(),
    };
    let size = msg.blob_size.unwrap_or(0);
    let local_path = files_dir.join(format!("{}-{}", msg.id, msg.body));
    let line = format!(
        "- {nick}  {filename}  ({size}B)  @file:{path}\n",
        filename = msg.body,
        path = local_path.display(),
    );
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())
        .with_context(|| format!("append {}", path.display()))?;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

/// Best-effort load of `self_nick` from `~/.cc-connect/config.json`. Any
/// missing/malformed config returns `None` (falls back to pubkey prefix).
fn load_self_nick() -> Option<String> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let path = home.join(".cc-connect").join("config.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("self_nick")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

async fn build_file_drop(
    store: &MemStore,
    path_str: &str,
    author_pubkey: &str,
    topic_id_hex: &str,
) -> Result<Message> {
    let path = std::path::Path::new(path_str);
    let abs_path = std::path::absolute(path)
        .with_context(|| format!("absolute path of {path_str}"))?;
    // Sensitive-path blocklist (SECURITY.md §"Mitigation today" /
    // §"Operating guidance"). Closes the credential-exfil prompt-inject
    // pivot; operators with a real need can opt out per process.
    let bypass = std::env::var("CC_CONNECT_DROP_ALLOW_DANGEROUS")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    if !bypass {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));
        if let DropSafety::Block { reason } = drop_safety::evaluate(&abs_path, &home) {
            return Err(anyhow!(
                "DROP_REFUSED: {reason} (set CC_CONNECT_DROP_ALLOW_DANGEROUS=1 to override)"
            ));
        }
    }
    let metadata = std::fs::metadata(&abs_path)
        .with_context(|| format!("stat {}", abs_path.display()))?;
    let size = metadata.len();
    if size > cc_connect_core::message::FILE_DROP_MAX_BYTES {
        return Err(anyhow!(
            "BLOB_TOO_LARGE: {} exceeds the {} byte cap",
            size,
            cc_connect_core::message::FILE_DROP_MAX_BYTES
        ));
    }
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("FILENAME_INVALID: cannot extract filename from {path_str:?}"))?
        .to_string();

    let tag = store
        .blobs()
        .add_path(&abs_path)
        .await
        .with_context(|| format!("add_path {}", abs_path.display()))?;
    let hash_hex = tag.hash.to_string();

    let id = new_ulid();
    let msg = Message::new_file_drop(
        &id,
        author_pubkey.to_string(),
        now_ms(),
        filename,
        hash_hex,
        size,
    )
    .context("build file_drop Message")?;

    let files_dir = files_dir_for(topic_id_hex);
    copy_local_to_files_dir(&msg, &abs_path, &files_dir)
        .context("save local copy for hook")?;
    Ok(msg)
}

fn copy_local_to_files_dir(
    msg: &Message,
    src: &std::path::Path,
    files_dir: &std::path::Path,
) -> Result<()> {
    std::fs::create_dir_all(files_dir)
        .with_context(|| format!("create_dir_all {}", files_dir.display()))?;
    let _ = std::fs::set_permissions(files_dir, std::fs::Permissions::from_mode(0o700));
    let target = files_dir.join(format!("{}-{}", msg.id, msg.body));
    if target.exists() {
        return Ok(());
    }
    std::fs::copy(src, &target)
        .with_context(|| format!("copy {} → {}", src.display(), target.display()))?;
    let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

/// Fetch a file_drop's blob from the author's NodeId and export it under
/// `<files_dir>/<id>-<filename>`. Idempotent — skips the download if the
/// destination file already exists.
pub(crate) async fn fetch_and_export_blob(
    store: &MemStore,
    endpoint: &Endpoint,
    msg: &Message,
    files_dir: &std::path::Path,
) -> Result<()> {
    let hash_hex = msg
        .blob_hash
        .as_deref()
        .ok_or_else(|| anyhow!("BLOB_HASH_MISSING for {}", msg.id))?;
    let hash = Hash::from_str(hash_hex)
        .map_err(|e| anyhow!("BLOB_HASH_PARSE: {hash_hex} ({e})"))?;
    let author_id = PublicKey::from_str(&msg.author)
        .map_err(|e| anyhow!("AUTHOR_PARSE: {} ({e})", msg.author))?;

    std::fs::create_dir_all(files_dir)
        .with_context(|| format!("create_dir_all {}", files_dir.display()))?;
    let _ = std::fs::set_permissions(files_dir, std::fs::Permissions::from_mode(0o700));
    let target = files_dir.join(format!("{}-{}", msg.id, msg.body));
    if target.exists() {
        return Ok(());
    }

    let downloader = store.downloader(endpoint);
    downloader
        .download(hash, Some(author_id))
        .await
        .with_context(|| format!("download blob {hash}"))?;
    store
        .blobs()
        .export(hash, &target)
        .await
        .with_context(|| format!("export {} → {}", hash, target.display()))?;
    let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600));
    Ok(())
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

// ---------- IPC unix-socket server -----------------------------------------

/// Pick an IPC socket path + write the marker file pointing at it.
///
/// Unix-domain sockets on macOS are capped at 104 bytes (SUN_LEN). A
/// straight `$TMPDIR/cc-connect-$UID/sockets/<64-hex>.sock` path blows
/// past that on macOS where `$TMPDIR` is `/var/folders/...`. So we put
/// the actual socket under `/tmp` with an 8-hex random tag (~24 byte
/// path) and store the absolute path in a marker file under HOME so
/// cc-connect-mcp can find it.
fn ipc_socket_path(topic_id_hex: &str) -> Result<(PathBuf, PathBuf)> {
    let uid = rustix::process::geteuid().as_raw();
    let mut rand_buf = [0u8; 4];
    getrandom::getrandom(&mut rand_buf)
        .map_err(|e| anyhow!("OS rng for socket suffix: {e}"))?;
    let mut rand_hex = String::with_capacity(8);
    for b in rand_buf {
        use std::fmt::Write as _;
        let _ = write!(rand_hex, "{b:02x}");
    }
    let socket_path = PathBuf::from(format!("/tmp/cc-{uid}-{rand_hex}.sock"));
    let _ = std::fs::remove_file(&socket_path);

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME not set"))?;
    let marker = home
        .join(".cc-connect")
        .join("rooms")
        .join(topic_id_hex)
        .join("chat.sock");
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    std::fs::write(&marker, socket_path.display().to_string())
        .with_context(|| format!("write {}", marker.display()))?;
    let _ = std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600));
    Ok((socket_path, marker))
}

/// Removes both the unix socket file and the HOME-side marker pointing at
/// it on Drop so the next chat session can rebind cleanly.
struct IpcSocketGuard {
    socket_path: PathBuf,
    marker_path: PathBuf,
}

impl IpcSocketGuard {
    fn new(socket_path: &Path, marker_path: &Path) -> Self {
        Self {
            socket_path: socket_path.to_path_buf(),
            marker_path: marker_path.to_path_buf(),
        }
    }
}

impl Drop for IpcSocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.marker_path);
    }
}

/// Drives one accepted IPC client.
///
/// Wire format: newline-delimited JSON, one command per line, one
/// response per command. Responses are a single JSON object with
/// `{"ok": bool}` plus optional `data` / `err`.
async fn ipc_server_loop(
    listener: tokio::net::UnixListener,
    local_input_tx: mpsc::Sender<String>,
    mcp_input_tx: mpsc::Sender<String>,
    log_path: PathBuf,
    files_dir: PathBuf,
    mention_state: std::sync::Arc<MentionState>,
) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(p) => p,
            Err(_) => return,
        };
        let local_input_tx = local_input_tx.clone();
        let mcp_input_tx = mcp_input_tx.clone();
        let log_path = log_path.clone();
        let files_dir = files_dir.clone();
        let mention_state = mention_state.clone();
        tokio::spawn(async move {
            handle_ipc_client(
                stream,
                local_input_tx,
                mcp_input_tx,
                log_path,
                files_dir,
                mention_state,
            )
            .await
        });
    }
}

async fn handle_ipc_client(
    stream: tokio::net::UnixStream,
    local_input_tx: mpsc::Sender<String>,
    mcp_input_tx: mpsc::Sender<String>,
    log_path: PathBuf,
    files_dir: PathBuf,
    mention_state: std::sync::Arc<MentionState>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line).await {
            Ok(n) => n,
            Err(_) => return,
        };
        if n == 0 {
            return;
        }
        let resp = dispatch_ipc(
            &line,
            &local_input_tx,
            &mcp_input_tx,
            &log_path,
            &files_dir,
            &mention_state,
        )
        .await;
        let mut out = serde_json::to_vec(&resp).unwrap_or_else(|_| b"{\"ok\":false,\"err\":\"encode\"}".to_vec());
        out.push(b'\n');
        if write_half.write_all(&out).await.is_err() {
            return;
        }
    }
}

#[derive(serde::Serialize)]
struct IpcResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    err: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

/// Hard cap on `wait_for_mention` block time, mirrors a sane MCP tool call
/// budget: long enough that Claude isn't woken twice a minute on quiet
/// rooms, short enough that a stuck call recovers without an obvious
/// hang. Claude re-arms on each return.
const WAIT_FOR_MENTION_MAX_MS: u64 = 600_000;
const WAIT_FOR_MENTION_DEFAULT_MS: u64 = 300_000;

/// Pick the input channel for an IPC `send`/`at`/`drop` based on the
/// optional `source` field in the request.
///
///   - `"human"` → Local channel (peers see the bare nick; owner
///     @-mention wakeups fire). chat-ui sets this.
///   - `"ai"` (default for backwards compat) → Mcp channel (peers see
///     `<nick>-cc`; treated as the AI's voice). cc-connect-mcp uses this
///     by omission.
fn pick_input_channel<'a>(
    payload: &serde_json::Value,
    local_input_tx: &'a mpsc::Sender<String>,
    mcp_input_tx: &'a mpsc::Sender<String>,
) -> &'a mpsc::Sender<String> {
    match payload.get("source").and_then(|x| x.as_str()) {
        Some("human") => local_input_tx,
        _ => mcp_input_tx,
    }
}

async fn dispatch_ipc(
    raw: &str,
    local_input_tx: &mpsc::Sender<String>,
    mcp_input_tx: &mpsc::Sender<String>,
    log_path: &Path,
    files_dir: &Path,
    mention_state: &std::sync::Arc<MentionState>,
) -> IpcResponse {
    let v: serde_json::Value = match serde_json::from_str(raw.trim()) {
        Ok(v) => v,
        Err(e) => {
            return IpcResponse {
                ok: false,
                err: Some(format!("PARSE_ERROR: {e}")),
                data: None,
            }
        }
    };
    let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
    match action {
        "send" => {
            let body = match v.get("body").and_then(|x| x.as_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => {
                    return IpcResponse {
                        ok: false,
                        err: Some("MISSING_BODY".into()),
                        data: None,
                    }
                }
            };
            let tx = pick_input_channel(&v, local_input_tx, mcp_input_tx);
            let _ = tx.send(body).await;
            ok_response()
        }
        "at" => {
            let nick = v.get("nick").and_then(|x| x.as_str()).unwrap_or("");
            let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("");
            if nick.is_empty() || body.is_empty() {
                return IpcResponse {
                    ok: false,
                    err: Some("MISSING_NICK_OR_BODY".into()),
                    data: None,
                };
            }
            let tx = pick_input_channel(&v, local_input_tx, mcp_input_tx);
            let _ = tx.send(format!("@{nick} {body}")).await;
            ok_response()
        }
        "drop" => {
            let path = match v.get("path").and_then(|x| x.as_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => {
                    return IpcResponse {
                        ok: false,
                        err: Some("MISSING_PATH".into()),
                        data: None,
                    }
                }
            };
            let tx = pick_input_channel(&v, local_input_tx, mcp_input_tx);
            let _ = tx.send(format!("/drop {path}")).await;
            ok_response()
        }
        "recent" => {
            let limit = v.get("limit").and_then(|x| x.as_u64()).unwrap_or(20) as usize;
            match recent_messages(log_path, limit) {
                Ok(msgs) => IpcResponse {
                    ok: true,
                    err: None,
                    data: Some(serde_json::json!({ "messages": msgs })),
                },
                Err(e) => IpcResponse {
                    ok: false,
                    err: Some(format!("{e:#}")),
                    data: None,
                },
            }
        }
        "list_files" => {
            let limit = v.get("limit").and_then(|x| x.as_u64()).unwrap_or(50) as usize;
            match list_files_in(files_dir, limit) {
                Ok(entries) => IpcResponse {
                    ok: true,
                    err: None,
                    data: Some(serde_json::json!({ "files": entries })),
                },
                Err(e) => IpcResponse {
                    ok: false,
                    err: Some(format!("{e:#}")),
                    data: None,
                },
            }
        }
        "save_summary" => {
            let text = match v.get("text").and_then(|x| x.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    return IpcResponse {
                        ok: false,
                        err: Some("MISSING_TEXT".into()),
                        data: None,
                    }
                }
            };
            // The summary lives next to log.jsonl: rooms/<topic>/summary.md.
            let dir = match log_path.parent() {
                Some(p) => p.to_path_buf(),
                None => {
                    return IpcResponse {
                        ok: false,
                        err: Some("NO_LOG_PARENT".into()),
                        data: None,
                    }
                }
            };
            match save_summary(&dir, &text) {
                Ok(()) => ok_response(),
                Err(e) => IpcResponse {
                    ok: false,
                    err: Some(format!("{e:#}")),
                    data: None,
                },
            }
        }
        "wait_for_mention" => {
            let since_id = v
                .get("since_id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            let timeout_ms = v
                .get("timeout_ms")
                .and_then(|x| x.as_u64())
                .unwrap_or(WAIT_FOR_MENTION_DEFAULT_MS)
                .min(WAIT_FOR_MENTION_MAX_MS);

            // Subscribe BEFORE scanning the buffer so any event arriving
            // between the buffer scan and the await is delivered through rx.
            let mut rx = mention_state.tx.subscribe();

            // Buffered replay: caller has a watermark and there are events
            // newer than it sitting in the ring. Return immediately.
            if let Some(ev) = mention_state.snapshot_after(since_id.as_deref()) {
                return IpcResponse {
                    ok: true,
                    err: None,
                    data: Some(serde_json::json!({
                        "mention": ev,
                        "reason": "buffered",
                    })),
                };
            }

            let timeout = std::time::Duration::from_millis(timeout_ms);
            match tokio::time::timeout(timeout, rx.recv()).await {
                Ok(Ok(ev)) => IpcResponse {
                    ok: true,
                    err: None,
                    data: Some(serde_json::json!({
                        "mention": ev,
                        "reason": "received",
                    })),
                },
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => IpcResponse {
                    ok: true,
                    err: None,
                    data: Some(serde_json::json!({
                        "mention": null,
                        "reason": "lagged",
                        "skipped": n,
                    })),
                },
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => IpcResponse {
                    ok: false,
                    err: Some("BROADCAST_CLOSED".into()),
                    data: None,
                },
                Err(_elapsed) => IpcResponse {
                    ok: true,
                    err: None,
                    data: Some(serde_json::json!({
                        "mention": null,
                        "reason": "timeout",
                    })),
                },
            }
        }
        other => IpcResponse {
            ok: false,
            err: Some(format!("UNKNOWN_ACTION: {other}")),
            data: None,
        },
    }
}

fn ok_response() -> IpcResponse {
    IpcResponse {
        ok: true,
        err: None,
        data: None,
    }
}

fn recent_messages(log_path: &Path, limit: usize) -> Result<Vec<serde_json::Value>> {
    let mut log_file = log_io::open_or_create_log(log_path)?;
    let all = log_io::read_since(&mut log_file, None)?;
    let take_n = all.len().saturating_sub(limit);
    let recent = &all[take_n..];
    Ok(recent
        .iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "author": m.author,
                "nick": m.nick,
                "ts": m.ts,
                "kind": m.kind,
                "body": m.body,
            })
        })
        .collect())
}

/// Write `text` atomically to `<room_dir>/summary.md`. Creates the file
/// (or overwrites) with mode 0600. Capped at 64 KiB — anything longer
/// is truncated rather than rejected, since the hook injection budget is
/// only a fraction of that anyway.
fn save_summary(room_dir: &Path, text: &str) -> Result<()> {
    const MAX_BYTES: usize = 64 * 1024;
    std::fs::create_dir_all(room_dir)
        .with_context(|| format!("create_dir_all {}", room_dir.display()))?;
    let path = room_dir.join("summary.md");
    let mut payload = if text.len() > MAX_BYTES {
        let mut s = text.as_bytes()[..MAX_BYTES].to_vec();
        s.extend_from_slice("\n\n…(truncated to 64 KiB)\n".as_bytes());
        s
    } else {
        text.as_bytes().to_vec()
    };
    if !payload.ends_with(b"\n") {
        payload.push(b'\n');
    }
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, &payload)
        .with_context(|| format!("write {}", tmp.display()))?;
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

fn list_files_in(files_dir: &Path, limit: usize) -> Result<Vec<serde_json::Value>> {
    if !files_dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(files_dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            if !m.is_file() {
                return None;
            }
            let mtime = m.modified().ok()?;
            Some((mtime, e.path()))
        })
        .collect();
    // Most recent first.
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    entries.truncate(limit);
    Ok(entries
        .into_iter()
        .map(|(mtime, path)| {
            let secs = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            serde_json::json!({
                "path": path.display().to_string(),
                "name": path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
                "size": size,
                "mtime": secs,
            })
        })
        .collect())
}

// ---------- active-rooms PID file ------------------------------------------

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

#[cfg(test)]
mod mention_state_tests {
    use super::*;

    fn ev(id: &str, body: &str) -> MentionEvent {
        MentionEvent {
            id: id.to_string(),
            ts: 0,
            nick: "alice".to_string(),
            body: body.to_string(),
        }
    }

    /// `snapshot_after(None)` MUST return None even with a populated buffer.
    /// Rationale: the very-first `cc_wait_for_mention` call carries no
    /// watermark; we don't want to replay events the hook already injected.
    #[test]
    fn snapshot_returns_none_when_no_cutoff_supplied() {
        let st = MentionState::new();
        st.push(ev("01H0000000000000000000000A", "hi"));
        assert!(st.snapshot_after(None).is_none());
    }

    /// With a watermark, snapshot returns the *oldest* buffered event
    /// strictly newer than it (FIFO catch-up across multiple unread).
    #[test]
    fn snapshot_returns_oldest_after_cutoff() {
        let st = MentionState::new();
        st.push(ev("01H0000000000000000000000A", "first"));
        st.push(ev("01H0000000000000000000000B", "second"));
        st.push(ev("01H0000000000000000000000C", "third"));
        let got = st
            .snapshot_after(Some("01H0000000000000000000000A"))
            .expect("expected the second event");
        assert_eq!(got.id, "01H0000000000000000000000B");
    }

    /// Watermark equal to or above the latest id → no buffered replay.
    #[test]
    fn snapshot_skips_when_caught_up() {
        let st = MentionState::new();
        st.push(ev("01H0000000000000000000000A", "first"));
        assert!(st.snapshot_after(Some("01H0000000000000000000000A")).is_none());
        assert!(st.snapshot_after(Some("01H0000000000000000000000Z")).is_none());
    }

    /// Push beyond capacity drops the oldest. Otherwise a noisy room would
    /// pin stale mentions in front of fresh ones forever.
    #[test]
    fn buffer_evicts_oldest_at_capacity() {
        let st = MentionState::new();
        for i in 0..(MENTION_BUFFER_CAP + 5) {
            st.push(ev(&format!("01H{:023}", i), "x"));
        }
        let buf = st.recent.lock().unwrap();
        assert_eq!(buf.len(), MENTION_BUFFER_CAP);
        // Oldest still present is index 5 (0..4 evicted).
        let first = buf.front().unwrap();
        assert_eq!(first.id, format!("01H{:023}", 5));
    }

    /// Live broadcast: a subscriber sees an event pushed AFTER it
    /// subscribed (the canonical wait_for_mention path).
    #[tokio::test]
    async fn broadcast_delivers_to_live_subscriber() {
        let st = MentionState::new();
        let mut rx = st.tx.subscribe();
        st.push(ev("01H0000000000000000000000A", "live"));
        let got = rx.recv().await.expect("subscriber should receive");
        assert_eq!(got.id, "01H0000000000000000000000A");
    }

    /// Best-effort: pushing with no subscribers MUST NOT error or panic.
    #[test]
    fn push_with_no_subscribers_is_noop() {
        let st = MentionState::new();
        st.push(ev("01H0000000000000000000000A", "x"));
        // No assertion needed — getting here is the point.
    }
}
