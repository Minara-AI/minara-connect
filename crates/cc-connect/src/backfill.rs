//! Backfill RPC — PROTOCOL.md §6.2 + ADR-0002.
//!
//! Lets a joining Peer fetch the last-N Messages from an already-online
//! peer's log so its Claude isn't dropped into mid-conversation.
//!
//! Wire format on the bidirectional iroh stream (ALPN `cc-connect/v1/backfill`):
//!   - 4-byte big-endian length prefix (BYTES of the JSON body)
//!   - UTF-8 JSON body of exactly that length
//!   - One request → one response → responder closes the stream.
//!
//! Both sides cap the length at 16 MiB and validate `v == 1`.

use anyhow::{anyhow, Context, Result};
use cc_connect_core::{log_io, message::Message};
use iroh::{
    endpoint::{Connection, RecvStream, SendStream},
    protocol::{AcceptError, ProtocolHandler},
    Endpoint, EndpointAddr,
};
use iroh_blobs::store::mem::MemStore;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::chat_session::fetch_and_export_blob;

/// ALPN identifying the cc-connect Backfill protocol. Must be byte-exact.
pub const BACKFILL_ALPN: &[u8] = b"cc-connect/v1/backfill";

/// 16 MiB hard cap on either side of the stream (PROTOCOL.md §6.2 anti-DoS).
const MAX_FRAME_BYTES: usize = 1 << 24;

/// Per-attempt timeout (PROTOCOL.md §6.2: "joiner MUST abandon a Backfill
/// that has not produced a complete response within 5 seconds").
const PER_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize, Deserialize)]
struct BackfillRequest {
    v: u32,
    /// Exclusive lower bound by ULID; `None` means "your latest `limit`".
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<String>,
    /// Capped at 50 by the server.
    limit: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackfillResponse {
    v: u32,
    messages: Vec<Message>,
}

const SERVER_LIMIT_CAP: u32 = 50;
const PROTO_V: u32 = 1;

// ---------------------------------------------------------------------------
// Server side — ProtocolHandler accepting an inbound Backfill stream.
// ---------------------------------------------------------------------------

/// State for a Backfill server handler. Reads from a per-Room log path on demand.
#[derive(Debug, Clone)]
pub struct BackfillHandler {
    log_path: Arc<PathBuf>,
}

impl BackfillHandler {
    pub fn new(log_path: PathBuf) -> Self {
        Self {
            log_path: Arc::new(log_path),
        }
    }
}

impl ProtocolHandler for BackfillHandler {
    fn accept(
        &self,
        connection: Connection,
    ) -> impl std::future::Future<Output = Result<(), AcceptError>> + Send {
        let log_path = Arc::clone(&self.log_path);
        async move {
            let result = handle_one(&log_path, connection).await;
            if let Err(e) = result {
                crate::gossip_debug::log("backfill server", &format!("handle_one err: {e:#}"));
                eprintln!("[backfill] server: {e:#}");
            }
            Ok(())
        }
    }
}

async fn handle_one(log_path: &Path, connection: Connection) -> Result<()> {
    crate::gossip_debug::log(
        "backfill server",
        &format!("accept log={}", log_path.display()),
    );
    let (mut send, mut recv) = connection.accept_bi().await.context("accept_bi")?;
    crate::gossip_debug::log("backfill server", "accept_bi ok");

    // Read length-prefixed request.
    let body = read_length_prefixed(&mut recv)
        .await
        .context("read request")?;
    crate::gossip_debug::log(
        "backfill server",
        &format!("read request body bytes={}", body.len()),
    );
    let request: BackfillRequest = serde_json::from_slice(&body).context("parse request")?;
    crate::gossip_debug::log(
        "backfill server",
        &format!(
            "parsed request v={} since={:?} limit={}",
            request.v, request.since, request.limit
        ),
    );

    if request.v != PROTO_V {
        return Err(anyhow!("VERSION_MISMATCH: request v={}", request.v));
    }
    let limit = request.limit.min(SERVER_LIMIT_CAP) as usize;

    // Read the log and produce the response set: messages with `id > since`,
    // ordered ascending, capped at `limit`.
    let mut log_file = log_io::open_or_create_log(log_path).context("open log")?;
    crate::gossip_debug::log("backfill server", "log opened");
    let all = log_io::read_since(&mut log_file, request.since.as_deref()).context("read_since")?;
    crate::gossip_debug::log(
        "backfill server",
        &format!("read_since returned {} message(s)", all.len()),
    );
    // Server returns the FIRST (oldest) `limit` qualifying messages so the
    // joiner sees a contiguous prefix of unread history. PROTOCOL §6.2:
    // "responder MUST return all matching Messages up to limit".
    let messages: Vec<Message> = all.into_iter().take(limit).collect();
    crate::gossip_debug::log(
        "backfill server",
        &format!(
            "returning {} message(s) after limit={}",
            messages.len(),
            limit
        ),
    );

    let response = BackfillResponse {
        v: PROTO_V,
        messages,
    };
    let resp_bytes = serde_json::to_vec(&response).context("encode response")?;
    crate::gossip_debug::log(
        "backfill server",
        &format!("encoded response bytes={}", resp_bytes.len()),
    );
    write_length_prefixed(&mut send, &resp_bytes)
        .await
        .context("write response")?;
    crate::gossip_debug::log("backfill server", "write_length_prefixed ok");
    send.finish().context("finish send stream")?;
    crate::gossip_debug::log("backfill server", "finish ok");
    // QUIC: `finish()` only schedules the FIN bit, it does NOT guarantee
    // the bytes have actually flushed to the peer. If we drop the
    // Connection (which happens when handle_one returns and the
    // ProtocolHandler::accept future ends), in-flight bytes are
    // discarded — the joiner sees "connection lost" mid-read of the
    // length prefix even though the server logged "finish ok".
    // `stopped().await` resolves once the peer has ACK'd the FIN, so
    // it's safe to drop the connection. Bound it so a misbehaving
    // joiner can't hold the handler open indefinitely.
    let _ = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;
    crate::gossip_debug::log("backfill server", "stopped ok");

    Ok(())
}

// ---------------------------------------------------------------------------
// Client side — open a Backfill stream to one peer with a hard timeout,
// write any received Messages to the local log (deduplicated by id).
// ---------------------------------------------------------------------------

/// Outcome of a single Backfill attempt.
#[derive(Debug)]
pub enum BackfillOutcome {
    /// Got a response. `appended` is the list of Messages persisted in
    /// id-ascending order — the caller surfaces them through the chat
    /// UI's normal incoming-message lane so the user sees history, not
    /// just the marker line. `spoof_dropped` counts entries we refused
    /// because their `author` matched our own Pubkey (PROTOCOL §4
    /// self-spoof guard) — caller surfaces a UI warning when non-zero.
    Filled {
        appended: Vec<Message>,
        spoof_dropped: usize,
    },
    /// Peer answered but the response was empty (peer's log had nothing newer).
    Empty,
    /// Per-attempt timeout fired.
    Timeout,
    /// Anything else: dial failure, malformed response, etc.
    Failed(String),
}

/// Try one peer. Will not retry. Caller decides whether to try the next peer
/// or surface the joined-late marker.
///
/// `self_pubkey` is the receiver's own canonical Pubkey string; messages
/// in the backfill response whose `author` equals it are dropped per
/// PROTOCOL §4 ("self-spoof prevention"). A malicious responder could
/// otherwise inject "history" claiming to be from us and our hook would
/// happily render it as if we'd authored it ourselves. Pass an empty
/// string to disable the check (tests only).
pub async fn try_backfill_from(
    endpoint: &Endpoint,
    store: &MemStore,
    peer: &EndpointAddr,
    since: Option<String>,
    log_path: &Path,
    files_dir: &Path,
    self_pubkey: &str,
) -> BackfillOutcome {
    match tokio::time::timeout(
        PER_ATTEMPT_TIMEOUT,
        attempt(
            endpoint,
            store,
            peer,
            since,
            log_path,
            files_dir,
            self_pubkey,
        ),
    )
    .await
    {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => BackfillOutcome::Failed(format!("{e:#}")),
        Err(_) => BackfillOutcome::Timeout,
    }
}

async fn attempt(
    endpoint: &Endpoint,
    store: &MemStore,
    peer: &EndpointAddr,
    since: Option<String>,
    log_path: &Path,
    files_dir: &Path,
    self_pubkey: &str,
) -> Result<BackfillOutcome> {
    let connection = endpoint
        .connect(peer.clone(), BACKFILL_ALPN)
        .await
        .context("connect to backfill peer")?;
    let (mut send, mut recv) = connection.open_bi().await.context("open_bi")?;

    let request = BackfillRequest {
        v: PROTO_V,
        since,
        limit: SERVER_LIMIT_CAP,
    };
    let req_bytes = serde_json::to_vec(&request).context("encode request")?;
    write_length_prefixed(&mut send, &req_bytes)
        .await
        .context("write request")?;
    send.finish().context("finish send stream")?;

    let body = read_length_prefixed(&mut recv)
        .await
        .context("read response")?;
    let response: BackfillResponse = serde_json::from_slice(&body).context("parse response")?;
    if response.v != PROTO_V {
        return Err(anyhow!("VERSION_MISMATCH: response v={}", response.v));
    }

    if response.messages.is_empty() {
        return Ok(BackfillOutcome::Empty);
    }

    // Dedup by id: read existing local ids, then append only Messages whose
    // id we haven't seen.
    let mut log_file = log_io::open_or_create_log(log_path).context("open log for dedup")?;
    let existing: HashSet<String> = log_io::read_since(&mut log_file, None)
        .context("read existing log for dedup")?
        .into_iter()
        .map(|m| m.id)
        .collect();

    let mut appended: Vec<Message> = Vec::new();
    let mut spoof_dropped = 0usize;
    for msg in response.messages {
        // PROTOCOL §4 self-spoof guard: a malicious responder must not
        // be able to inject "history" attributed to us. Counted (not
        // logged here) so the caller surfaces a single visible warning
        // through the chat UI's normal Warn channel.
        if !self_pubkey.is_empty() && msg.author == self_pubkey {
            spoof_dropped += 1;
            continue;
        }
        if existing.contains(&msg.id) {
            continue;
        }
        // file_drop Messages: dial the original author's NodeId via
        // iroh-blobs to fetch the bytes, then export them locally so the
        // hook's `@file:` path resolves on the next prompt. If the author
        // is offline we drop the announcement (better than logging a
        // pointer the user can't follow).
        if msg.kind == cc_connect_core::message::KIND_FILE_DROP {
            if let Err(e) = fetch_and_export_blob(store, endpoint, &msg, files_dir).await {
                eprintln!(
                    "[backfill] fetch blob for {} failed: {e:#} (skipping)",
                    msg.id
                );
                continue;
            }
        }
        log_io::append(&mut log_file, &msg).context("append backfilled Message")?;
        appended.push(msg);
    }
    Ok(BackfillOutcome::Filled {
        appended,
        spoof_dropped,
    })
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

async fn read_length_prefixed(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| anyhow!("read length prefix: {e}"))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(anyhow!(
            "FRAME_TOO_LARGE: length prefix {len} > cap {}",
            MAX_FRAME_BYTES
        ));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| anyhow!("read body of {len} bytes: {e}"))?;
    Ok(body)
}

async fn write_length_prefixed(send: &mut SendStream, body: &[u8]) -> Result<()> {
    if body.len() > MAX_FRAME_BYTES {
        return Err(anyhow!(
            "FRAME_TOO_LARGE: body {} > cap {}",
            body.len(),
            MAX_FRAME_BYTES
        ));
    }
    let len = (body.len() as u32).to_be_bytes();
    send.write_all(&len).await?;
    send.write_all(body).await?;
    Ok(())
}
